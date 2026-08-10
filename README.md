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
