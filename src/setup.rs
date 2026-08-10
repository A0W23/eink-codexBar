use std::env;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use codex_zectrix_dashboard::{
    AppServerClient, DashboardConfig, ObservedDashboardState, ObservedQuota, QuotaCache,
    render_dashboard,
};
use reqwest::blocking::Client;
use reqwest::blocking::multipart::{Form, Part};
use reqwest::header::{HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

const KEYCHAIN_SERVICE: &str = "com.barrybarrywu.codex-zectrix-dashboard";
const KEYCHAIN_ACCOUNT: &str = "zectrix-api-key";
const NOTE4_BOARD: &str = "bread-compact-wifi";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Settings {
    device_id: String,
    page_id: u8,
    privacy_mode: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Device {
    device_id: String,
    alias: String,
    board: String,
}

#[derive(Deserialize)]
struct ApiResponse<T> {
    code: i64,
    data: T,
}

#[derive(Deserialize)]
struct ApiCodeResponse {
    code: i64,
}

pub fn run_setup() -> Result<(), Box<dyn std::error::Error>> {
    let data_dir = data_dir()?;
    fs::create_dir_all(&data_dir)?;
    let settings_path = data_dir.join("settings.json");
    let previous_settings_bytes = fs::read(&settings_path).ok();
    let previous_settings: Option<Settings> = previous_settings_bytes
        .as_deref()
        .and_then(|bytes| serde_json::from_slice(bytes).ok());
    let keychain = Keychain::from_environment();
    let existing_key = keychain.find()?;
    let (api_key, replace_key) = match existing_key {
        Some(key) if prompt_yes_no("使用 macOS 钥匙串中的现有 API Key？", true)? => {
            (key, false)
        }
        _ => (read_api_key()?, true),
    };

    let zectrix = ZectrixClient::new(&api_key)?;
    let devices = zectrix.discover_devices()?;
    if devices.is_empty() {
        return Err("没有发现可用的 ZECTRIX 设备".into());
    }
    println!("可用的 NOTE4 设备：");
    for (index, device) in devices.iter().enumerate() {
        println!(
            "  {}. {}  {}  {}",
            index + 1,
            device.alias,
            device.device_id,
            device.board
        );
    }
    let default_device = previous_settings
        .as_ref()
        .and_then(|settings| {
            devices
                .iter()
                .position(|device| device.device_id == settings.device_id)
        })
        .map(|index| index + 1)
        .unwrap_or(1);
    let device_index = prompt_number("选择设备", 1, devices.len(), default_device)? - 1;
    let page_id = prompt_number(
        "选择持久页面 pageId",
        1,
        5,
        previous_settings
            .as_ref()
            .map_or(1, |settings| usize::from(settings.page_id)),
    )? as u8;
    let privacy_mode = prompt_yes_no(
        "隐藏任务标题（隐私模式）？",
        previous_settings
            .as_ref()
            .is_some_and(|settings| settings.privacy_mode),
    )?;

    let preview_path = data_dir.join("pending-preview.png");
    render_pending_preview(&data_dir, &preview_path, privacy_mode)?;
    println!("已生成待上传预览：{}", preview_path.display());
    if privacy_mode {
        println!("隐私说明：任务标题已隐藏；状态和数量仍会作为图像像素上传到 ZECTRIX Cloud。");
    } else {
        println!("隐私说明：可见任务标题将作为图像像素上传到 ZECTRIX Cloud。");
    }
    if !prompt_yes_no("确认用该预览替换选定设备页面？", false)? {
        println!("已取消，未上传图像。");
        return Ok(());
    }

    let image = fs::read(&preview_path)?;
    if replace_key {
        keychain.store(&api_key)?;
    }
    write_settings(
        &settings_path,
        &Settings {
            device_id: devices[device_index].device_id.clone(),
            page_id,
            privacy_mode,
        },
    )?;
    if let Err(error) = zectrix.upload_image(&devices[device_index].device_id, page_id, image) {
        if matches!(error, UploadError::Rejected(_)) {
            restore_settings(&settings_path, previous_settings_bytes.as_deref())?;
        }
        return Err(error.into());
    }
    println!("首次看板已上传，设置已保存。");
    Ok(())
}

fn render_pending_preview(
    data_dir: &Path,
    preview_path: &Path,
    privacy_mode: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = env::var_os("CODEX_ZECTRIX_CODEX_BIN")
        .map(AppServerClient::new)
        .unwrap_or_default();
    let cache_path = data_dir.join("quota.json");
    let last_known = fs::read(&cache_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<ObservedQuota>(&bytes).ok());
    let mut cache = QuotaCache::new(last_known);
    let quota = match client.read_quota() {
        Ok(quota) => {
            let quota = cache.update::<std::convert::Infallible>(Ok(quota))?;
            fs::write(&cache_path, serde_json::to_vec(&quota)?)?;
            quota
        }
        Err(error) => cache.update(Err(error))?,
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_secs()
        .try_into()?;
    let output = render_dashboard(
        ObservedDashboardState {
            quota,
            tasks: Vec::new(),
            prompt: None,
            response: None,
            reasoning: None,
            project_path: None,
            tool: None,
            error_text: None,
            plan: None,
        },
        now,
        DashboardConfig {
            privacy_mode,
            previous_frame_hash: None,
        },
    )?;
    output.frame.write_png(preview_path)?;
    Ok(())
}

struct ZectrixClient {
    client: Client,
    base_url: String,
}

#[derive(Debug, thiserror::Error)]
enum UploadError {
    #[error("{0}")]
    Rejected(String),
    #[error("ZECTRIX 未确认上传结果；设备页面可能已更改，请重新运行 setup 核对")]
    ResultUnknown,
}

impl ZectrixClient {
    fn new(api_key: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let mut headers = HeaderMap::new();
        headers.insert(
            "X-API-Key",
            HeaderValue::from_str(api_key).map_err(|_| "API Key 格式无效")?,
        );
        Ok(Self {
            client: Client::builder()
                .default_headers(headers)
                .timeout(std::time::Duration::from_secs(30))
                .build()?,
            base_url: env::var("CODEX_ZECTRIX_API_BASE")
                .unwrap_or_else(|_| "https://cloud.zectrix.com".into())
                .trim_end_matches('/')
                .to_owned(),
        })
    }

    fn discover_devices(&self) -> Result<Vec<Device>, Box<dyn std::error::Error>> {
        let response = self
            .client
            .get(format!("{}/open/v1/devices", self.base_url))
            .send()?;
        if !response.status().is_success() {
            return Err(format!("ZECTRIX 设备发现失败：HTTP {}", response.status()).into());
        }
        let response: ApiResponse<Vec<Device>> = response.json()?;
        if response.code != 0 {
            return Err(format!("ZECTRIX 设备发现失败：API code {}", response.code).into());
        }
        Ok(response
            .data
            .into_iter()
            .filter(|device| device.board == NOTE4_BOARD)
            .collect())
    }

    fn upload_image(
        &self,
        device_id: &str,
        page_id: u8,
        image: Vec<u8>,
    ) -> Result<(), UploadError> {
        if image.len() > 2 * 1024 * 1024 {
            return Err(UploadError::Rejected("ZECTRIX 图像超过 2 MB 限制".into()));
        }
        let image = Part::bytes(image)
            .file_name("codex-dashboard.png")
            .mime_str("image/png")
            .map_err(|_| UploadError::Rejected("无法构造 PNG 上传".into()))?;
        let form = Form::new()
            .text("dither", "false")
            .text("pageId", page_id.to_string())
            .part("images", image);
        let response = self
            .client
            .post(format!(
                "{}/open/v1/devices/{device_id}/display/image",
                self.base_url
            ))
            .multipart(form)
            .send()
            .map_err(|_| UploadError::ResultUnknown)?;
        if response.status().is_client_error() {
            return Err(UploadError::Rejected(format!(
                "ZECTRIX 图像上传失败：HTTP {}",
                response.status()
            )));
        }
        if !response.status().is_success() {
            return Err(UploadError::ResultUnknown);
        }
        let response: ApiCodeResponse = response.json().map_err(|_| UploadError::ResultUnknown)?;
        if response.code != 0 {
            return Err(UploadError::Rejected(format!(
                "ZECTRIX 图像上传失败：API code {}",
                response.code
            )));
        }
        Ok(())
    }
}

struct Keychain {
    command: PathBuf,
}

impl Keychain {
    fn from_environment() -> Self {
        Self {
            command: env::var_os("CODEX_ZECTRIX_SECURITY_BIN")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/usr/bin/security")),
        }
    }

    fn find(&self) -> Result<Option<Zeroizing<String>>, Box<dyn std::error::Error>> {
        let output = Command::new(&self.command)
            .args([
                "find-generic-password",
                "-a",
                KEYCHAIN_ACCOUNT,
                "-s",
                KEYCHAIN_SERVICE,
                "-w",
            ])
            .stderr(Stdio::null())
            .output()?;
        if !output.status.success() {
            return Ok(None);
        }
        let value = String::from_utf8(output.stdout)?;
        let value = value.trim_end_matches(['\r', '\n']).to_owned();
        if value.is_empty() {
            return Ok(None);
        }
        Ok(Some(Zeroizing::new(value)))
    }

    fn store(&self, api_key: &str) -> Result<(), Box<dyn std::error::Error>> {
        let mut child = Command::new(&self.command)
            .args([
                "add-generic-password",
                "-a",
                KEYCHAIN_ACCOUNT,
                "-s",
                KEYCHAIN_SERVICE,
                "-U",
                "-w",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let mut input = child.stdin.take().ok_or("无法写入 macOS 钥匙串")?;
        input.write_all(api_key.as_bytes())?;
        input.write_all(b"\n")?;
        drop(input);
        if !child.wait()?.success() {
            return Err("无法将 API Key 保存到 macOS 钥匙串".into());
        }
        Ok(())
    }
}

fn read_api_key() -> Result<Zeroizing<String>, Box<dyn std::error::Error>> {
    print!("请输入 ZECTRIX API Key（输入不会显示）：");
    io::stdout().flush()?;
    let value = if io::stdin().is_terminal() {
        rpassword::read_password()?
    } else {
        let mut value = String::new();
        io::stdin().read_line(&mut value)?;
        value.trim_end_matches(['\r', '\n']).to_owned()
    };
    if value.is_empty() {
        return Err("API Key 不能为空".into());
    }
    Ok(Zeroizing::new(value))
}

fn prompt_yes_no(prompt: &str, default: bool) -> Result<bool, Box<dyn std::error::Error>> {
    let suffix = if default { "[Y/n]" } else { "[y/N]" };
    let answer = prompt_line(&format!("{prompt} {suffix} "))?;
    if answer.is_empty() {
        return Ok(default);
    }
    match answer.to_ascii_lowercase().as_str() {
        "y" | "yes" => Ok(true),
        "n" | "no" => Ok(false),
        _ => Err("请输入 y 或 n".into()),
    }
}

fn prompt_number(
    prompt: &str,
    minimum: usize,
    maximum: usize,
    default: usize,
) -> Result<usize, Box<dyn std::error::Error>> {
    let answer = prompt_line(&format!("{prompt} [{default}]："))?;
    let value = if answer.is_empty() {
        default
    } else {
        answer.parse()?
    };
    if !(minimum..=maximum).contains(&value) {
        return Err(format!("请输入 {minimum} 到 {maximum} 之间的数字").into());
    }
    Ok(value)
}

fn prompt_line(prompt: &str) -> Result<String, Box<dyn std::error::Error>> {
    print!("{prompt}");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(answer.trim().to_owned())
}

fn data_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(path) = env::var_os("CODEX_ZECTRIX_DATA_DIR") {
        return Ok(PathBuf::from(path));
    }
    let home = env::var_os("HOME").ok_or("无法确定用户主目录")?;
    Ok(PathBuf::from(home)
        .join("Library/Application Support")
        .join("codex-zectrix-dashboard"))
}

fn write_settings(path: &Path, settings: &Settings) -> Result<(), Box<dyn std::error::Error>> {
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(settings)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn restore_settings(
    path: &Path,
    previous: Option<&[u8]>,
) -> Result<(), Box<dyn std::error::Error>> {
    match previous {
        Some(previous) => fs::write(path, previous)?,
        None if path.exists() => fs::remove_file(path)?,
        None => {}
    }
    Ok(())
}
