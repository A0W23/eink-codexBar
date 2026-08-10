# Build the companion as a Rust binary

The bundled companion will be a precompiled Rust executable responsible for app-server observation, state normalization, image rendering, Keychain access, and ZECTRIX publishing. Rust was chosen to satisfy the zero-runtime installation requirement while preserving a path to Windows or Linux builds; Python and Node would add user-managed runtimes, while Swift would unnecessarily bind the core to macOS.
