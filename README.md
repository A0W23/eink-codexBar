# Codex Dashboard for ZECTRIX

在 macOS 上生成固定的 400×300 NOTE4 示例预览：

```sh
cargo run --release -- preview --input fixtures/sample-dashboard.json --output preview.png
```

从当前 Codex 额度生成实时预览：

```sh
cargo run --release -- live-preview --output live-preview.png
```

`live-preview` 会启动官方独立 `codex app-server`，初始化只读客户端并请求 `account/rateLimits/read`。程序只保留额度百分比、窗口时长、重置时间和结构化的重置额度数量；同一路径再次运行时，若数据源不可用，会沿用本地的上次额度并标记为可能过期。该命令不会连接 Codex Desktop 或 ZECTRIX 设备。

正式插件会内置编译后的 companion 二进制文件，用户不需要安装 Python、Node.js 或 Rust。

## 安装公开插件

```sh
codex plugin marketplace add BarryBarrywu/codex-zectrix-dashboard --ref v0.1.0
codex plugin add codex-zectrix-dashboard@codex-zectrix-dashboard
```

安装后在 Codex 中使用 `$setup-zectrix-dashboard`。API Key 只通过本机无回显终端输入；setup 会先生成预览并披露上传边界，确认后才进行首次推送。

## 发布验证

```sh
./scripts/build-release.sh
./scripts/test-clean-install.sh
cargo test --locked --all-features
```

自动化验证、分发包验证与 NOTE4 实机验证分别报告。fixture 或 fake ZECTRIX 通过不代表已完成实机可读性、设备选择、持久 `pageId` 或后续真实推送验证。
