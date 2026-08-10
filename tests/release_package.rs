use std::fs;
use std::process::Command;

const APPROVED_NAME: &str = "Codex Dashboard for ZECTRIX";
const APPROVED_SLUG: &str = "codex-zectrix-dashboard";
const APPROVED_DESCRIPTION: &str = "Show Codex quota and task status on ZECTRIX NOTE4";

#[test]
fn public_release_is_an_installable_marketplace_with_a_universal_companion() {
    let marketplace: serde_json::Value = serde_json::from_slice(
        &fs::read(".agents/plugins/marketplace.json").expect("marketplace manifest"),
    )
    .unwrap();
    assert_eq!(marketplace["name"], APPROVED_SLUG);
    assert_eq!(marketplace["plugins"][0]["name"], APPROVED_SLUG);
    assert_eq!(marketplace["plugins"][0]["source"]["source"], "local");
    assert_eq!(marketplace["plugins"][0]["source"]["path"], "./plugin");

    let plugin: serde_json::Value = serde_json::from_slice(
        &fs::read("plugin/.codex-plugin/plugin.json").expect("plugin manifest"),
    )
    .unwrap();
    assert_eq!(plugin["name"], APPROVED_SLUG);
    assert_eq!(plugin["description"], APPROVED_DESCRIPTION);
    assert_eq!(plugin["interface"]["displayName"], APPROVED_NAME);
    assert_eq!(plugin["skills"], "./skills/");
    assert_eq!(plugin["license"], "MIT");

    let binary = "plugin/bin/codex-zectrix-dashboard";
    let output = Command::new("/usr/bin/lipo")
        .args(["-archs", binary])
        .output()
        .unwrap();
    assert!(output.status.success());
    let architectures = String::from_utf8(output.stdout).unwrap();
    assert!(architectures.contains("arm64"));
    assert!(architectures.contains("x86_64"));

    let linked_frameworks = Command::new("/usr/bin/otool")
        .args(["-L", binary])
        .output()
        .unwrap();
    assert!(linked_frameworks.status.success());
    assert!(
        String::from_utf8(linked_frameworks.stdout)
            .unwrap()
            .contains("/System/Library/Frameworks/Security.framework/")
    );

    let output = Command::new(binary).arg("version").output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        env!("CARGO_PKG_VERSION")
    );

    let expected_fingerprint = Command::new("./scripts/source-fingerprint.sh")
        .output()
        .unwrap();
    assert!(expected_fingerprint.status.success());
    let packaged_fingerprint = Command::new(binary)
        .arg("build-fingerprint")
        .output()
        .unwrap();
    assert!(packaged_fingerprint.status.success());
    assert_eq!(packaged_fingerprint.stdout, expected_fingerprint.stdout);
}

#[test]
fn repository_and_distributed_plugin_include_license_and_release_limits() {
    let repository_license = fs::read_to_string("LICENSE").expect("repository MIT license");
    let packaged_license = fs::read_to_string("plugin/LICENSE").expect("packaged MIT license");
    assert_eq!(repository_license, packaged_license);
    assert!(repository_license.contains("MIT License"));

    let notes = fs::read_to_string("plugin/RELEASE_NOTES.md").expect("release notes");
    for limitation in [
        "Desktop unread blue dot",
        "待你",
        "authoritative 检查",
        "plan progress",
        "task mutation",
        "other operating systems",
        "other display models",
    ] {
        assert!(
            notes.contains(limitation),
            "missing limitation: {limitation}"
        );
    }
}

#[test]
fn packaged_companion_runs_without_python_node_or_rust_on_path() {
    let temp = tempfile::tempdir().unwrap();
    let output = Command::new("plugin/bin/codex-zectrix-dashboard")
        .args([
            "preview",
            "--input",
            "fixtures/sample-dashboard.json",
            "--output",
        ])
        .arg(temp.path().join("preview.png"))
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let image = image::open(temp.path().join("preview.png"))
        .unwrap()
        .to_luma8();
    assert_eq!(image.dimensions(), (400, 300));
}
