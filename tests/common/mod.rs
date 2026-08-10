pub fn dashboard_binary() -> std::path::PathBuf {
    std::env::var_os("CODEX_ZECTRIX_TEST_BINARY")
        .map(Into::into)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_codex-zectrix-dashboard").into())
}
