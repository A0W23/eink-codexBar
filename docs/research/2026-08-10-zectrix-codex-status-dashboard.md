# ZECTRIX Codex 状态看板调研

调研日期：2026-08-10

范围：只使用 ZECTRIX 官方网站/Wiki、OpenAI 官方源码，以及项目作者自己的 GitHub 仓库。结论中的“插件”指可以安装或运行的软件；官方页面上的效果图不等同于已有可下载插件。

## 结论

1. 名称确实是 **ZECTRIX**，不是 Xteink 或 Zetrix。目标设备应为 **ZECTRIX NOTE4**：4.2 英寸、400×300 黑白电子纸、ESP32-S3。NOTE4C 是四色开放开发型号。[ZECTRIX 官网](https://www.zectrix.com/)、[NOTE4 产品页](https://www.zectrix.com/en/note4.html)
2. 截至本次检索，**没有找到一个可以直接安装、同时覆盖“ZECTRIX + Codex 额度 + 当前任务 + 任务/步骤完成状态”的完整插件**。ZECTRIX 官网确实展示了 NOTE4 上的 Codex 使用量画面，但没有为该画面链接代码或安装包，因此只能证明场景存在，不能证明已有公开插件。[ZECTRIX 官网](https://www.zectrix.com/)
3. 已有项目里最接近需求的是 [627150795/codex-eink-dashboard](https://github.com/627150795/codex-eink-dashboard)：它已经展示 Codex Desktop 任务标题、运行/等待/完成/失败、计划步骤进度和额度；但当前只验证 Windows + 212×104 SKD-CLOCK BLE 屏，不支持 ZECTRIX，且仓库未声明许可证。
4. 最可靠的数据路线不是解析网页或直接复用 `~/.codex/auth.json` 凭据请求内部 URL，而是使用 OpenAI 官方 `codex app-server`：额度读取有 `account/rateLimits/read`，任务列表和实时状态有 `thread/list`、`thread/status/changed`、`turn/started`、`turn/completed`、`turn/plan/updated`。官方 stdio transport 是正式支持的；WebSocket 明确标为 experimental/unsupported。[OpenAI app-server README](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md)
5. ZECTRIX 端不需要刷固件。官方 Open API 已支持获取设备、推送 1–5 张图片或文本、指定持久页面和删除页面；最自然的实现是本机守护进程生成 400×300 单色图，再通过 Open API 推送。[ZECTRIX Open API](https://wiki.zectrix.com/zh/software/api-docs)

## 1. ZECTRIX 是什么，所谓“插件”是什么

### 硬件与开放方式

- NOTE4 是面向成品体验的黑白 AI 电子墨水屏；NOTE4C 是四色开放开发设备。官网将 NOTE4 定位为可显示任务、天气和自有数据的常显设备。[官网](https://www.zectrix.com/)、[NOTE4](https://www.zectrix.com/en/note4.html)
- 官方 Open API 基础地址是 `https://cloud.zectrix.com/open/v1/`，支持 Bearer token 或 `X-API-Key`。图片推送接口为 `POST /open/v1/devices/{deviceId}/display/image`，支持最多 5 张、每张不超过 2 MB，并可设置 `dither` 与 `pageId`。[API 文档](https://wiki.zectrix.com/zh/software/api-docs)
- 设备按同步周期主动拉取新画面，因此“本机已经推送”与“屏幕已经物理刷新”之间会有延迟。[官方 FAQ](https://wiki.zectrix.com/zh/help/faq)

### 插件规范的实际边界

ZECTRIX 官方 Wiki 把基于官方固件 API 的第三方项目归入“官方固件插件”，但公开接口本质上是云端 REST API，不是一个要求固定 manifest 或在设备内运行的插件 ABI。对本项目而言，一个“ZECTRIX 插件”可以是：

1. 本机读取 Codex 数据；
2. 生成 400×300 单色图片；
3. 使用官方 API Key 推送至指定 NOTE4 页面。

官方 Wiki 收录了桌面客户端、信息推送脚本、LLM Skill 和 Barry 的 Claude Code 看板，说明这种本机/云端桥接方式属于现有生态的正常形态。[官方社区项目索引](https://wiki.zectrix.com/zh/software/Community-OpenSource)

### 官方仓库

没有找到由 ZECTRIX 官方公开维护、可直接 clone 的统一固件或插件 SDK GitHub 仓库。官方开源路线图说明 Phase 1 的基础固件、协议、原理图与结构文件当前需要向客服申请；语音待办应用固件与业务平台尚未开放。因此不能把任一社区 GitHub 仓库称为“ZECTRIX 官方仓库”。[官方开源路线图](https://wiki.zectrix.com/zh/software/opensource)

## 2. 是否已有可用的 ZECTRIX Codex 插件

| 项目 | 能做什么 | 判断 |
| --- | --- | --- |
| [BarryBarrywu/claude-eink-bridge](https://github.com/BarryBarrywu/claude-eink-bridge) | 已验证 Claude Code HUD 到 ZECTRIX 的 400×300 渲染、API 推送、变更去重和后台生命周期；也已被 ZECTRIX 官方 Wiki 收录 | **ZECTRIX 输出层可直接运行；Codex 输入层不可直接用。** 仓库包含 MIT License，可在保留许可声明的前提下复用 |
| [627150795/codex-eink-dashboard](https://github.com/627150795/codex-eink-dashboard) | Codex Desktop 额度、侧栏标题、运行/等待/完成/失败、未读提醒、计划步骤进度 | **功能最接近，但不能直接用于 ZECTRIX/macOS。** 当前要求 Windows 与 SKD-CLOCK BLE 屏；仓库未声明许可证 |
| [boybook/crs-ink-dashboard](https://github.com/boybook/crs-ink-dashboard) | ZECTRIX 400×300 上显示 Codex/Claude quota snapshot | **条件式可用。** 数据依赖 `claude-relay-service` 管理端，不读取本机 Codex 当前任务；README 标明其 Codex quota endpoint 不是 OpenAI 官方 API；仓库包含 MIT License |
| [chimon89/tokens-cli-zectrix](https://github.com/chimon89/tokens-cli-zectrix) | 使用 ZECTRIX Open API 显示 Tokens CLI 的累计 token、排名、session 等 | **不是本需求。** 不是 Codex ChatGPT 订阅额度或任务状态；仓库未声明许可证 |
| [MDR-EX1000/ZECTRIX-PLUGIN](https://github.com/MDR-EX1000/ZECTRIX-PLUGIN) | ZECTRIX token 仪表盘模板 | **不是本需求。** 当前 provider 是 Kimi/DeepSeek，不是 Codex；仓库未声明许可证 |
| [kkkdkk/zectrix-skill](https://github.com/kkkdkk/zectrix-skill) | 封装设备查询、图片/文本推送、页面和待办操作 | **可直接用来验证 ZECTRIX API，但不采集 Codex 数据。** 仓库未声明许可证 |

所以，现阶段没有“一键安装即满足全部需求”的 ZECTRIX 插件。最接近的组合是：自行实现 Codex 数据/状态采集，再复用 MIT 授权的 `claude-eink-bridge` ZECTRIX 输出模式。无许可证的 Codex 参考仓库只用于理解思路，不直接复制代码。

## 3. 可复用或可借鉴的 Codex HUD / status / usage 项目

### 优先级 A：直接回答本项目核心问题

#### `codex-eink-dashboard`

[原始仓库](https://github.com/627150795/codex-eink-dashboard)已经验证了需求本身可行。其 README 明确列出任务标题、状态、计划进度、完成/失败/未读和主额度；源码则显示额度优先通过 `codex app-server` 的 `account/rateLimits/read` 获取，失败时才从 rollout JSONL 的 `rate_limits` 回退。任务标题和状态仍依赖 `session_index.jsonl`、`state_5.sqlite`、`logs_2.sqlite`、`.codex-global-state.json` 与 rollout JSONL，因此这部分属于对 Codex 本地存储实现的适配，不是稳定的官方展示 API。[额度采集源码](https://github.com/627150795/codex-eink-dashboard/blob/main/src/codex_eink/quota.py)、[会话采集源码](https://github.com/627150795/codex-eink-dashboard/blob/main/src/codex_eink/sessions.py)

适合借鉴：状态归并、计划进度呈现、未读完成提醒、事件合并、图片 hash 去重。

不宜直接复制：设备 BLE 协议、Windows 启动方式、本地数据库 schema 依赖；仓库也没有许可证。

#### `codex-buddy`

[openelab-commits/codex-buddy](https://github.com/openelab-commits/codex-buddy)是可安装的 Codex plugin + 本地桥，展示 5h/7d 额度和 `busy`、`idle`、`attention`、`completed` 状态，还把 `PermissionRequest` 转发到 M5Stack StickS3。它证明插件可以用 hooks 启动常驻本地桥。截至本次复核，本机 Codex CLI 0.146.1 已将 `plugins`、`hooks` 和 `plugin_sharing` 标为 stable，旧 README 中启用 `plugin_hooks` 实验开关的步骤已过时。但 Desktop 跨任务可见性仍需实机验证，不应把单一 hook 当作唯一状态源。仓库未声明许可证。

适合借鉴：Codex 插件封装、状态机、低带宽设备消息结构、权限等待态。

#### `codelight`

[henrikekblad/codelight](https://github.com/henrikekblad/codelight)支持 Codex CLI/IDE 的状态、usage、permissions、questions 和 conversation，并输出到 GeekMagic 屏、Android、GNOME、KDE、VS Code。它的状态模型和多客户端 companion 架构很接近本项目，但设备通道不是 ZECTRIX。仓库包含 MIT License，可以依法复用并保留许可声明。

### 优先级 B：额度采集或任务语义

- [steipete/CodexBar](https://github.com/steipete/CodexBar)：成熟的 macOS/CLI Codex 额度展示，支持 session/weekly windows、reset countdown 和 JSON/脚本使用。适合参考额度模型和刷新策略，不提供 Codex 当前任务进度。仓库包含 MIT License；其 Codex provider 文档说明优先使用 Codex OAuth API 或本机 Codex CLI，并把网页 dashboard cookie 数据作为可选增强。[Codex provider 文档](https://github.com/steipete/CodexBar/blob/main/docs/codex.md)
- [feelgood3000/codex-task-monitor-kindle](https://github.com/feelgood3000/codex-task-monitor-kindle)：从 `~/.codex` session files 生成只读任务/项目 JSON API 与 Kindle 黑白页面，60 秒局部刷新；非常适合参考电子纸任务信息层级和只读 LAN 服务，但没有额度，也不是 ZECTRIX 推送。仓库包含 MIT License。
- [justbp/codex-task-inventory](https://github.com/justbp/codex-task-inventory)：macOS 本地 Codex Kanban，额度走 app-server，名称走 `thread/read`，运行/完成/中断/进展仍从 `state_5.sqlite` 与 rollout JSONL 归并；“进行中 → 待 Review → 人工确认完成”的任务语义最成熟。适合借鉴状态模型，但同时依赖私有本地 schema，而且仓库未声明许可证。
- [zellux/quote0-token-usage-dash](https://github.com/zellux/quote0-token-usage-dash)：把 Codex 5h/weekly 额度推到 Quote/0 墨水屏，适合参考 1-bit 布局和刷新；但它直接从 `~/.codex/auth.json` 取凭据，耦合认证文件与内部 endpoint，不如 app-server 稳定，且仓库未声明许可证。
- [shanggqm/codexU](https://github.com/shanggqm/codexU)：macOS 上同时展示 quota 与“今日任务”（活跃、待继续、定时、今日归档），任务分组语义值得参考，但没有 ZECTRIX 输出。仓库包含 MIT License。
- [pilipilisbot/trmnl-codex-usage-dashboard](https://github.com/pilipilisbot/trmnl-codex-usage-dashboard)：Codex usage 的 TRMNL 私有插件模板，适合参考电子纸 Liquid 版式和 webhook payload，只有额度没有本机任务。仓库包含 MIT License。

### 与 Barry 旧项目的关系

本地 [`claude-hud-main`](/Volumes/990%20EP/Dev/墨水屏/claude-hud-main) 是 Claude Code statusline HUD：它接收 Claude Code 官方 statusline stdin，再解析 transcript JSONL 得到 tools、agents 和 todos。可保留其“采集 → 统一状态 → 渲染”的分层思路，但 Claude 的 stdin/statusline contract 不能直接移植给 Codex。真正可参考的 ZECTRIX 端是 [BarryBarrywu/claude-eink-bridge](https://github.com/BarryBarrywu/claude-eink-bridge)。

## 4. 数据来源：官方稳定接口与本地推断的边界

| 需要展示的数据 | 推荐来源 | 稳定性判断 |
| --- | --- | --- |
| ChatGPT Codex 主/次额度、使用百分比、窗口长度、重置时间、credits、plan type | `codex app-server` → `account/rateLimits/read`；变化时接收 `account/rateLimits/updated` | **官方接口。** schema 明确含 `primary`、`secondary`、`usedPercent`、`windowDurationMins`、`resetsAt`、credits 与 plan type。[官方 README](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md#auth-endpoints)、[官方 schema](https://github.com/openai/codex/blob/main/codex-rs/app-server-protocol/schema/json/v2/GetAccountRateLimitsResponse.json) |
| 任务/thread 列表、标题、更新时间、当前是否运行 | 同一 app-server 连接使用 `thread/list`；实时监听 `thread/status/changed` | **官方接口。** `ThreadStatus` 为 `notLoaded`、`idle`、`systemError` 或 `active`；active 还含 waiting flags。[官方 README](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md#example-track-thread-status-changes)、[官方 schema](https://github.com/openai/codex/blob/main/codex-rs/app-server-protocol/schema/json/v2/ThreadStatusChangedNotification.json) |
| 一轮任务开始、完成、失败或中断 | `turn/started`、`turn/completed` | **官方接口。** 完成事件状态为 `completed`、`interrupted` 或 `failed`。[官方事件文档](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md#turn-events) |
| 计划步骤及每步 pending / in progress / completed | `turn/plan/updated` | **官方接口。** 事件直接返回 plan entries 与三态 status。[官方事件文档](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md#turn-events) |
| 正在执行的命令、文件修改、MCP/tool 状态 | `item/started`、各类 delta、`item/completed` | **官方接口。** 可用于生成“正在测试 / 正在编辑”等简短活动描述，但不应展示完整命令或 prompt。[官方 Item 文档](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md#items) |
| Codex Desktop 里“未读完成”或 UI 专属侧栏状态 | Desktop 私有状态文件或数据库，例如 `.codex-global-state.json`、`state_5.sqlite`、`logs_2.sqlite` | **本地实现细节，不稳定。** 官方 app-server schema 没有承诺 Desktop UI 的 unread contract；应用升级可能改名或改 schema |
| 历史任务状态、额度 fallback | `~/.codex/sessions/**/rollout-*.jsonl` | **可推断但不稳定。** 可作为降级路径和调试证据，不应作为唯一真源 |
| 直接读取 `~/.codex/auth.json` 并请求内部 ChatGPT endpoint | 若第三方项目这样实现，只作为兼容性参考 | **不建议。** 涉及敏感 token，并绑定非公开 endpoint/认证格式；官方 app-server 已提供受支持的账户读取面 |

### 一个关键限制

启动一个独立 `codex app-server --listen stdio://` 很适合读取账户额度，也适合由这个 app-server 自己承载的 threads；但它不必然自动订阅已经由 Codex Desktop 另一个进程加载的实时任务。官方文档说明 `thread/list` 对未在当前 app-server 进程加载的 thread 默认返回 `notLoaded`。若要精确观察 **正在运行的 Codex Desktop 任务**，应优先验证能否通过官方 control socket 连接 Desktop 正在使用的 app-server；官方提供的 `codex app-server proxy` 会连接 `$CODEX_HOME/app-server-control/app-server-control.sock`。如果当前 Desktop 版本没有开放可用 control socket，才需要以只读方式组合本地 rollout/SQLite 适配层。[官方 transport 文档](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md#protocol)、[thread/list 文档](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md#api-overview)

## 5. 建议的实现边界

建议把新项目定义为一个 **macOS 本机 companion + ZECTRIX Open API 输出器**，而不是修改 NOTE4 固件：

1. `CodexProvider`：优先连接官方 app-server/control socket，读取 quota、threads、turn 与 plan events；
2. `DesktopFallback`：只有官方实时连接不可用时，才只读 rollout/SQLite，并把结果标记为 inferred/stale；
3. `DashboardState`：统一为 quota、active tasks、waiting/approval、plan progress、completed/failed；
4. `Renderer`：输出 400×300 1-bit image；
5. `ZectrixPublisher`：使用官方 Open API 推到固定 `pageId`，用 hash 去重并限制刷新频率。

首个技术验证不应直接做完整插件，而应回答一个风险最大的具体问题：**当前 macOS Codex Desktop 是否能通过官方 app-server control socket 被只读订阅，从而同时获得正在运行 thread、plan 与完成事件。** 如果答案为否，再采用 `codex-eink-dashboard` 已验证过的本地文件/数据库观察策略。

## 6. 分类汇总

### 可直接用

- ZECTRIX 官方 Open API：设备发现、图片/文本推送、持久页面管理。
- OpenAI `codex app-server`：额度读取，以及由同一 server 加载的 thread/turn/plan 实时事件。
- `claude-eink-bridge`：可直接作为现有 ZECTRIX Claude 看板继续运行，但不能直接显示 Codex。

### 可借鉴

- `codex-eink-dashboard`：需求与状态模型最接近，但需重做 macOS/ZECTRIX 适配且注意无许可证。
- `codex-buddy`、`codelight`：Codex 状态机、权限等待和实体设备 companion。
- `CodexBar`：额度模型和刷新策略。
- `codex-task-monitor-kindle`：MIT 授权的黑白任务页与本地只读 JSON API。
- `codex-task-inventory`、`codexU`：任务分组、待 Review 和人工确认语义；前者未检测到许可证，后者包含 MIT License。
- `quote0-token-usage-dash`、TRMNL 模板：电子纸布局与低频刷新。

### 不可验证或不可直接宣称

- 不可宣称官网的 Codex 用量效果图对应一个已公开插件。
- 不可宣称社区项目是 ZECTRIX 官方仓库。
- 不可把 Codex Desktop 的 SQLite、global state 和 rollout JSONL schema 当作稳定公开 API。
- 不推荐把 CRS 管理端快照、直接读取 `auth.json` 后访问私有 endpoint，或仅解析 rollout 的推断结果包装成“官方 Codex 实时接口”。
- 对未声明许可证的 GitHub 仓库，可以阅读与独立重现思路；未经作者许可不应直接复制代码进入新项目。
