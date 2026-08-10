use std::fs;

#[test]
fn plugin_hooks_invoke_only_the_bundled_companion_hook_recorder() {
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read("plugin/hooks/hooks.json").unwrap()).unwrap();
    let hooks = manifest["hooks"].as_object().unwrap();

    assert_eq!(
        hooks.keys().cloned().collect::<Vec<_>>(),
        [
            "PermissionRequest",
            "PostToolUse",
            "PreToolUse",
            "SessionEnd",
            "SessionStart",
            "Stop",
            "UserPromptSubmit"
        ]
    );
    for definitions in hooks.values() {
        let command = definitions[0]["hooks"][0]["command"].as_str().unwrap();
        assert!(command.ends_with("codex-zectrix-dashboard\" hook-record"));
        assert!(!command.contains("python"));
        assert!(!command.contains("node"));
    }
}
