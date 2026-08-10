# Codex Dashboard for ZECTRIX

Generate the deterministic 400×300 NOTE4 sample preview on macOS:

```sh
cargo run --release -- preview --input fixtures/sample-dashboard.json --output preview.png
```

The production plugin will bundle the compiled companion binary; users will not need Python, Node.js, or Rust. This preview slice reads only the repository fixture and does not access Codex state, credentials, the network, or a ZECTRIX device.
