# Store the ZECTRIX API key in macOS Keychain

The setup flow will collect the ZECTRIX API key through a local non-echoing prompt and store it in macOS Keychain, while non-sensitive device and page settings remain in the plugin data directory. Passing the key through a Codex conversation or keeping it in plaintext configuration was rejected to prevent credentials from appearing in transcripts, tool output, logs, screenshots, or the public repository.
