use std::process::Command;

#[test]
fn preview_command_writes_a_400_by_300_monochrome_png() {
    let output_dir = tempfile::tempdir().unwrap();
    let output_path = output_dir.path().join("dashboard.png");
    let status = Command::new(env!("CARGO_BIN_EXE_codex-zectrix-dashboard"))
        .args([
            "preview",
            "--input",
            "fixtures/sample-dashboard.json",
            "--output",
            output_path.to_str().unwrap(),
        ])
        .status()
        .unwrap();

    assert!(status.success());
    let image = image::open(output_path).unwrap().to_luma8();
    assert_eq!(image.dimensions(), (400, 300));
    assert!(image.pixels().all(|pixel| matches!(pixel[0], 0 | 255)));
}
