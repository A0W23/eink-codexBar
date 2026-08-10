use std::process::Command;

#[test]
fn preview_command_writes_a_400_by_300_monochrome_png() {
    let output_dir = tempfile::tempdir().unwrap();
    let first_path = output_dir.path().join("first.png");
    let second_path = output_dir.path().join("second.png");
    for output_path in [&first_path, &second_path] {
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
    }

    assert_eq!(
        std::fs::read(&first_path).unwrap(),
        std::fs::read(&second_path).unwrap()
    );
    let image = image::open(first_path).unwrap().to_luma8();
    assert_eq!(image.dimensions(), (400, 300));
    assert!(image.pixels().all(|pixel| matches!(pixel[0], 0 | 255)));
}
