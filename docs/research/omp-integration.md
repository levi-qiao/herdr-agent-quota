# omp（oh-my-pi）接入研究

> 研究日期：2026-09-01（Asia/Shanghai）
> 复核版本：omp `18.0.11`（`https://omp.sh/install` 下载的 darwin-arm64 独立二进制，本机实测）；
> 源码 `can1357/oh-my-pi` main（2026-08-31 21:30 UTC，MIT，28.7k star）；
> herdr `0.8.2`（内置 `omp` integration，v8）；对照 pi `0.70.6` 与本仓库 `src/pi.rs`。
> 实验方式：`HOME` 指向临时目录安装并运行，未触碰用户真实 `~/.omp`、未做任何登录。

> **实施状态（2026-09-01）**：本文的方案 A 已经实现，见 `src/omp.rs`、
> `src/providers/omp.rs` 与 CHANGELOG 的 Unreleased 段。落地时相对下文有两处
> 改动：(1) 账户身份与 API key 判定全部来自 `omp usage --json`
> （report 的 `metadata` 带 email/accountId/orgId/projectId），**不读
> `agent.db`**，因此不碰任何 OAuth token；(2) context window 走 `models.db`
> 的只读 rusqlite 查询，而不是再 spawn 一次 `omp models --json` —— rusqlite
> 本来就是本仓库的依赖，而 model catalog 不含机密。Anthropic OAuth 已登录并做了
> 实测，但其 usage endpoint 当前返回 HTTP 429，因而尚未取得真实 Anthropic quota report。

## 结论先行

1. **omp 是 pi 的 fork，会话文件这一层可以直接复用。** 会话仍是 JSONL，
   `CURRENT_SESSION_VERSION = 3`，仍是 `id`/`parentId` 树、`model_change` 条目用
   `"provider/modelId"`、assistant 消息带 usage。`src/pi.rs` 的
   `parse_session_file` / `active_branch` / `active_model` / 上下文与缓存统计基本可原样用。
   文件名仍是 `<timestamp>_<sessionId>.jsonl`，`filename_matches_session_id` 的现有写法已兼容。
2. **凭证与模型目录这一层不能复用。** omp 把 pi 的 `auth.json`、`models.json`、
   `models-store.json` 全部搬进了 SQLite：`~/.omp/agent/agent.db`（`auth_credentials`）
   与 `~/.omp/agent/models.db`（`model_cache`）。`src/pi.rs` 里读这两个 JSON 的分支对 omp 不成立。
3. **omp 自带完整的用量层，这是与 pi 最大的差别。** `omp usage --json` 输出所有已认证账户的
   归一化 limit（5h / 7d / monthly，`usedFraction`、`resetsAt`、`status`），并把结果**持久化**
   缓存在 `agent.db` 的 `cache` 表（`usage_cache:` 前缀，TTL 5 分钟 + last-good 长期保底）。
   因此 omp 不应照搬 pi「只做路由、借 canonical collector」的模型，而应作为
   **自带 collector 的 harness**（凭证域 = omp 自己的 store）接入。
4. **归属精度反而比 pi 高。** omp 会在会话文件里写 `credential_pin` 条目，值是
   `sha256(provider\0accountId\0email\0orgId\0projectId)`。`omp usage --json` 的每份 report
   带同样这几个身份字段，所以「这个 pane 用的是多账户中的哪一个」是可判定的，不需要靠猜。

## 一手证据

### herdr 侧

`herdr integration status` 已内置 omp（herdr 0.8.2）：

```
omp: not installed (/Users/<user>/.omp/agent/extensions/herdr-omp-agent-state.ts)
```

从 herdr 二进制里取出的该扩展（`HERDR_INTEGRATION_ID=omp`，`VERSION=8`）与 pi 版同源，
上报形状为 `pane.report_agent_session` / `pane.report_agent`，`agent: "omp"`，
会话引用优先 `agent_session_path`（`sessionManager.getSessionFile()`，绝对路径），
退化到 `agent_session_id`。也就是说 `src/herdr.rs` 现有的 session 解析路径无需改造，
只要 `Harness::from_agent_name` 认识 `"omp"`。

### 磁盘布局（实测）

```
~/.omp/agent/agent.db       # 凭证 + usage 缓存 + usage_history + client_usage（WAL）
~/.omp/agent/models.db      # model_cache（每个 provider 一行，models 列是 JSON）
~/.omp/agent/sessions/...   # 按 cwd 分目录，<timestamp>_<id>.jsonl；子 agent 写 <parent>/<agentId>.jsonl
~/.omp/logs/ ~/.omp/natives/
```

`agent.db` 相关表（实测 `.schema`）：

```sql
CREATE TABLE auth_credentials (id, provider, credential_type, data, disabled_cause, identity_key, created_at, updated_at);
CREATE TABLE cache (key TEXT PRIMARY KEY, value TEXT, expires_at INTEGER);        -- usage_cache:* 落在这里
CREATE TABLE usage_history (recorded_at, provider, account_key, email, account_id,
                            limit_id, label, window_label, used_fraction, status, resets_at);
```

**路径不能写死。** `DirResolver` 支持 `PI_CONFIG_DIR` 覆盖目录名、`--profile`
（`~/.omp/profiles/<name>/agent`）、以及 Linux/macOS 上 `omp config init-xdg` 之后的
`$XDG_DATA_HOME/omp`。有两个可靠的定位手段：`omp config path` 打印当前 agent 目录；
或者直接从 herdr 给的绝对会话路径反推（`…/agent/sessions/<...>.jsonl`）。
后者更稳，因为 action 跑在 herdr 服务端环境里，看不到 pane 自己的 `PI_CONFIG_DIR` / profile
（见 AGENTS.md「插件 action 看不到调用方环境」一节）。

### CLI 接口与耗时（本机实测，空凭证）

| 命令 | 用途 | 耗时 |
|---|---|---|
| `omp usage --json` | 全部账户的归一化 limit | 0.31–0.33s |
| `omp usage --json --provider anthropic` | 单 provider | 同上 |
| `omp usage invalidate [--provider p]` | 作废用量缓存 | — |
| `omp models --json` | 每个模型的 `contextWindow` / `maxTokens` / `cost` | 0.37–0.39s |
| `omp config path` | 当前 agent 目录 | — |

空凭证时 `omp usage --json` 返回：

```json
{"generatedAt":1788219943563,"reports":[],"accountsWithoutUsage":[],"disabledCredentials":[],"capacity":{}}
```

有凭证时 `reports[]` 是 `UsageReport`（`provider`、`fetchedAt`、`limits[]`，`raw` 已被剥掉），
`limits[]` 是 `UsageLimit`：`id`（如 `anthropic:5h`、`anthropic:7d`、`anthropic:7d:opus`）、
`label`、`scope{provider,accountId,orgId,projectId,modelId,...}`、
`window{id,label,durationMs,resetsAt}`、`amount{used,limit,usedFraction,unit}`、`status`。
xAI 侧的 id 是 `1w` / `1mo`，Codex 侧 provider id 是 `openai-codex`。
`resolveUsedFraction()` 的优先级（显式 fraction > used/limit > percent > 1-remaining）
在 `packages/ai/src/usage.ts` 里是导出的公共约定。

**缓存行为是这个方案能成立的关键**：`omp usage` 走的是
`authStorage.fetchUsageReports()`（不带 forceRefresh），命中 `AuthStorageUsageCache`，
而该 cache 的后端就是 `agent.db` 的 `cache` 表 —— 跨进程持久。所以我们每次 spawn
`omp usage --json` **不等于**每次打一次 provider 的用量端点：5 分钟 TTL（带 ±25% 抖动）
内是纯本地读，超时才回源，失败还有 last-good 兜底。omp 自己的注释写明这是因为
Anthropic / OpenAI 的 `/usage` 按 IP 限流。

### 与本仓库现有 provider 的对齐

`omp usage --json` 直接覆盖了我们四个 collector 里的三个（anthropic / openai-codex / xai），
外加 gemini、github-copilot、cursor、opencode-go、kimi、zai、minimax、antigravity 等
（`packages/ai/src/usage/` 下 19 个实现）。窗口口径也对得上：`anthropic:5h`
（`durationMs = 5h`）与 `anthropic:7d`（`durationMs = 7d`）正好映射
`WindowKind::FiveHour` / `WindowKind::Weekly`；monthly 类窗口按 AGENTS.md 的既有约定
**不参与** `quota_headroom` 与低额告警。

## 方案对比

| | A. 走 `omp usage --json` 子进程（推荐） | B. 直接读 `agent.db` | C. 照搬 pi：身份匹配后借 canonical collector |
|---|---|---|---|
| 数据来源 | omp 公开 CLI | 私有 SQLite schema | 我们已有的 claude/codex/grok collector |
| 覆盖范围 | omp 支持的全部 provider | 同左 | 只覆盖「omp 账户 == canonical CLI 账户」的情况 |
| 网络成本 | 命中 omp 5 分钟持久缓存；未命中才回源 | 零（但可能读到过期值） | 复用现有 collector，不增加 |
| 延迟 | ~0.3s/次（可按 provider 过滤） | ~ms | 0 |
| 新依赖 | 无（`std::process`） | `rusqlite`（bundled，C 代码）+ WAL 并发读 | 无 |
| 脆弱点 | CLI flag / JSON 形状变动 | 表结构与 `usage_cache:` 编码变动、bun 写 WAL 时的锁 | 用户在 omp 里登录的是第二个账号时直接失效 |
| 归属 | report 带 accountId/email，可与 `credential_pin` 对上 | 同左 | 只能靠 accountId 相等，omp 里拿不到（在 SQLite 里） |

**推荐 A**，理由三条：一是 omp 的凭证在自己的 store 里，C 方案在多账户下会给出错误归属，
而错误的配额数字比没有数字更糟；二是 A 的网络与限流问题已经被 omp 自己解决了（持久缓存 +
last-good），我们再叠一层 `CacheStore` 就足够；三是 B 省下的 0.3s 换来的是绑定一个私有
schema，而 omp 迭代很快（v18，日更），不值。B 可作为后续优化，前提是先把 A 的行为固定成契约测试。

## 实施草图

1. `src/model.rs`：`Harness::Omp`，`from_agent_name` 认 `"omp"`；`billing()` 返回 `None`（同 pi）。
   新增 `CredentialScope::OMP_STORE`，`BillingTarget::omp(provider)`，
   `collector_id` 形如 `omp.omp-store`，避免与 canonical 的 0.2 文件名冲突（与 opencode-go 同构）。
   → 验证：`cargo test`，新增 provider_contracts 用例。
2. `src/omp.rs`（新）：会话解析从 `src/pi.rs` 抽公共部分。**先抽再加**：把 JSONL v3 的分支/
   模型/usage/缓存统计逻辑提成 `session_jsonl` 模块，pi 与 omp 各自保留自己的
   凭证与模型目录读法。AGENTS.md 的规矩是「两个真实调用方再抽象」——这里正好是两个。
   → 验证：pi 现有测试全绿 + omp 用同构 fixture。
3. `src/providers/omp.rs`（新）：`omp usage --json` 解析成 `ProviderSnapshot`。
   要点：只取 `durationMs` 能识别的 5h / 7d 窗口；`usedFraction` 缺失时按
   `resolveUsedFraction` 的同一优先级回退；任何字段缺失/类型不对就丢掉这个窗口而不是猜 0%
   （与 `opencode_go.rs` 的 fail-closed 风格一致）；`status` 不参与判断。
   → 验证：用真实输出录 fixture 做解析测试。
4. `src/route.rs`：`Harness::Omp` 分支 —— 会话文件给出 provider/model/context，
   `credential_pin` 给出账户 hash，与 `omp usage --json` 的 report 身份字段
   （`sha256(provider\0accountId\0email\0orgId\0projectId)`）比对，命中则
   `Resolution::Subscription(BillingTarget::omp(provider))`，否则 `Indeterminate`。
   API key 凭证 → `NoSubscription`。
   → 验证：单元测试覆盖「同 provider 两个账户」这一 pi 无法处理的场景。
5. 上下文百分比：`omp models --json` 里的 `contextWindow`，按 `provider/id` 匹配，
   结果进 `CacheStore`（模型目录变动很慢，TTL 可给到小时级）。
   注意 pi 那条「存在 `models.json` 就不发布百分比」的保守规则对 omp 不适用：
   omp 的 CLI 输出已经是**合成后**的有效值。
6. 安装面：`src/cli.rs` 的 agent 解析、`src/prefs.rs` 的选择集合、`install.sh` /
   `uninstall.sh` 的 agent 列表、`src/configure/integration.rs` 的
   `integration_id(Harness::Omp) => Some("omp")`（herdr 侧 id 就是 `omp`）、
   `herdr-plugin.toml` 若有按 agent 的枚举。**特别注意** AGENTS.md 记过的那个坑：
   uninstall 的选择只能走 prefs 文件，不能走环境变量。
   → 验证：`tests/configure_round_trip.rs`、`tests/plugin_manifest.rs`。
7. 展示层：`Provider` 展示名沿用被路由到的 provider（Claude/Codex/Grok/…），
   凭证域标注 omp，与 opencode-go 的处理保持一致。

## 与「读写 pane 不是免费的」这条铁律的关系

这套方案不新增任何 `herdr pane read`。omp 的所有证据都来自本地文件和自身 CLI，
事件路径的权限表不变。唯一新增的是每次 refresh 最多一次 `omp usage --json` 子进程 ——
**必须按刷新轮次调用一次，而不是按 pane**（同一 provider 的多个 omp pane 共用一份 snapshot），
并且要进 `CacheStore`，TTL 不小于 omp 自己的 5 分钟，否则 watch 脉冲（默认 60s）会
每分钟 spawn 一次 bun 进程。`quota_headroom` 与低额告警的既有规则原样适用。

## 已验证项（2026-09-01，用户登录 omp 后实测）

在一个真实的 omp v18.0.11 pane（SuperGrok + ChatGPT + Cursor + Antigravity 四个
账户）上跑通了整条链路，同时修正了三处只读源码没发现的、与 Pi 不同的地方：

| 项 | 读源码的预期 | 实测 | 处理 |
|---|---|---|---|
| xAI 订阅的 provider id | `xai` | **`xai-oauth`**（`xai` 是 API key 路径） | 只映射 `xai-oauth` |
| `model_change` 条目 | `provider` + `modelId`（Pi 的写法） | **`model: "xai-oauth/grok-4.6"` 加 `role`** | 共享 parser 两种写法都认，且只认 `role=default` |
| transcript 首行 | 直接是 `session` header | 前面多一条 **`title` 记录，没有 `id`/`parentId`** | 解析时跳过，否则整个 branch 走不通 |

`credential_pin` 的摘要口径**逐字节对上了**：
`sha256("xai-oauth\0aef2514e-…\0<email>\0\0")` 与 transcript 里记录的
`bd751891daab…` 完全相同，多账户归属因此是可判定的，不是推测。

端到端结果（真实 session 文件 + 真实 `omp usage --json`）：identity
`Grok/grok-4.6`、context 5.17%（omp 自己的状态栏显示 5%）、cache 命中 99.6%、
7d 剩余 41%、重置时间与 report 一致。

`omp usage --json` 的另外两个发现，都已固化成契约测试
（`tests/fixtures/omp/usage-redacted.json` 是 `--redact` 后的真实输出）：

- SuperGrok 的同一个周窗口会报两条 limit（`xai-oauth:credits:1w` 与
  `xai-oauth:product:grokbuild:1w`），「限定词最少者胜」的规则选中了前者。
- **Antigravity 报的是 daily 池，Cursor 报的是没有窗口的消费额**。两者都没有侧边栏
  可放的槽位，因此整条丢弃而不是塞进周窗口——把按天的数字显示成按周的，比空着更糟。

## 仍未验证的项

以下仍需 Anthropic usage endpoint 恢复后才能收尾：

1. 已确认 omp 能识别本机 Anthropic OAuth 登录，但 `omp usage --json --provider
   anthropic` 把账号列在 `accountsWithoutUsage`，脱敏日志显示 usage 请求为 HTTP
   429。因而仍缺真实 Anthropic limit payload。源码里预期是 `anthropic:5h` /
   `anthropic:7d`，另有
   `anthropic:7d:opus` 这类按模型的子池——「限定词最少者胜」正是为它写的，但没实测过）。
   → 等冷却后只运行一次 `omp usage --json --redact --provider anthropic`；不要用紧密轮询测试。
2. 缓存行为实测只比较两次间隔足够长的 `omp usage --json` 结果与耗时。
   `agent.db` 含活跃 OAuth token，插件和诊断流程都不得直接打开它。
3. omp 子 agent 会话写在 `<parent>/<agentId>.jsonl`，pane 上报的是父还是子未确认。
   → 触发一次 subagent 后看事件里的路径。

## 开源横向对照

按「配额数据从哪来」分四类，本仓库已经各占一格：

| 模式 | 代表 | 本仓库对应 | 代价 |
|---|---|---|---|
| 宿主推送（statusline hook） | Claude Code statusLine | `src/providers/claude.rs` | 零轮询，但依赖宿主愿意推 |
| 读本地凭证 + 自己打官方用量端点 | CodexBar、`opencode_go` | `src/providers/*.rs` | 要自己扛限流与 schema 漂移 |
| **消费 agent 自带的用量层** | **omp `usage --json`** | 本文推荐的新格 | 依赖对方 CLI 契约，但限流/缓存/多 provider 都由对方解决 |
| 解析会话/日志自行估算 | ccusage 一类 | 未采用 | 只有 token 花费，拿不到订阅窗口剩余量 |

第三类是新出现的：pi 时代还没有用量层，所以 `src/pi.rs` 只能做路由；omp 把这块补齐了，
接入方式也就应该跟着变。反过来说，omp 的 `usage` 层本身是个可借鉴的归一化设计
（`UsageLimit{scope,window,amount,status}` + `resolveUsedFraction` 的回退优先级），
和我们 `model.rs` 里的 `UsageWindow`/`WindowKind` 是同一类抽象，映射不会有阻抗。
