# 侧栏额度行能否按宽度动态换行

研究日期：2026-08-31（Asia/Shanghai）
范围：Herdr 0.8.0 官方配置/CLI/API 合同、本仓库现有拼接布局、截图里的截断现象。
来源约束：Herdr 官方文档、本机 `herdr 0.8.0` 二进制与 `api schema`/`api snapshot`、本仓库 `src/configure/herdr.rs` / `src/herdr.rs`。不把社区插件 README 当 Herdr 合同，只作对照。

## 结论先行

**不能。** Herdr 0.8.0 没有按侧栏宽度自动换行、也没有把侧栏宽度交给插件做动态排版。`rows` 里每个内层数组固定画一行；同行多个有值的 token 用 `·` 拼起来；放不下就截断加省略号。空 token 和空行会消失，这是**内容自适应**（缺字段折叠），不是**宽度自适应**（窄了换行）。

截图里的 `cache... · no c...` 和 `5h 10... · 7d 8...` 正是这个合同：cache/error、5h/7d 被放在同一行，窄侧栏下两个 token **各自**被截断，然后中间再插 `·`。单独一行的 `context 5%` 反而完整，因为那一行只有一个 token。

现有拼接逻辑（cache+TTL、5h 空时 7d 折到 context、5h 在时 5h+7d 同行）不能“通用地按宽度重排”。插件能通用复用的只有 Herdr 已经提供的空 token / 空行折叠。若要让宽窄侧栏都读得完，只能改布局：**一个有值字段一行**，让空行自己消失。真正的动态换行要等 Herdr 做。

> **实现更新（2026-08-31）：** 安装期二选一，不是运行时按宽度换行。`packed` 保持现状拼接；`stacked` 把 `$quota_provider`、`$quota_model`、cache / TTL / context / 5h / 7d 各放一行。模型空时那一行折叠。发布规则不变（5h 空时 7d 仍走 `quota_week_inline_*`），stacked 只是把 inline 和 limits 两套 week token 放在同一行，有值的那套显示。选择经 `configure --sidebar-layout` / `HERDR_AGENT_QUOTA_SIDEBAR_LAYOUT` 写入 plugin state。

| 能力 | 状态 | 依据 |
| --- | --- | --- |
| 侧栏行按宽度软换行 | `unsupported` | 配置把每个内层数组画成一行；#1108 仍在要 wrapping，未进 0.8.0 |
| token 值里写 `\n` 强制换行 | `unsupported` | CLI 规范化会去掉控制字符，再把 token 截到 80 字符 |
| 查询当前侧栏宽度 | `unsupported` | snapshot 无 `sidebar_width`；`layout.updated` 是窗格布局，不是侧栏 |
| 侧栏拖宽事件 | `unsupported` | 插件事件列表无 sidebar resize |
| 空 token / 空行折叠 | `confirmed` | 官方配置文档 |
| 同行 token 用 `·` 拼接、无分隔符配置 | `confirmed` | 官方配置文档 |
| 样式字段 | `confirmed`：只有 `fg` / `bold` / `dim` | 默认配置注释与配置参考 |

状态含义：`confirmed` = 一手合同明确有；`unsupported` = 一手合同明确没有或明确做反面；`unknown` = 本机未做人眼对照的渲染细节（例如每个 token 分到多少列），但不改变“没有换行 API”的结论。

## 截图对应的是哪段拼接

当前安装写入的 agent 行（`~/.config/herdr/config.toml`，由 `append_quota_rows` 生成）是：

1. `state_icon` + `tab` + `$quota_provider_model`
2. `$quota_topic`
3. `$quota_cache` + `$quota_cache_ttl` + `$quota_error`
4. `$quota_context` + `$quota_week_inline_*`
5. `$quota_5h_*` + `$quota_week_*`

发布规则（`week_style_base`）：

- 5h 有值：7d 走 `$quota_week_*`，出现在第 5 行，形如 `5h 100% 3h34m · 7d 82% 6d0h`。
- 5h 为空（Grok / 周配额-only Codex）：7d 走 `$quota_week_inline_*`，出现在第 4 行，形如 `context 5% · 7d 82% 6d0h`。第 5 行若只剩空 5h 则整行消失。
- cache TTL 过期：`$quota_cache_ttl` 清空，`$quota_error` = `no cached`，第 3 行变成 `cache 0.0% · no cached`。

2026-08-31 对本机 `herdr api snapshot` 的实值（与截图同一类 Claude pane）：

| token | 值 | 显示宽 |
| --- | --- | --- |
| `quota_cache` | `cache 0.0%` | 10 |
| `quota_error` | `no cached` | 9 |
| `quota_context` | `context 5%` | 10 |
| `quota_5h_normal` | `5h 100% 3h34m` | 13 |
| `quota_week_warning` | `7d 82% 6d0h` | 11 |

拼起来：

- cache 行：`cache 0.0% · no cached` = 22 列
- 额度行：`5h 100% 3h34m · 7d 82% 6d0h` = 27 列
- context 单独：`context 5%` = 10 列

官方默认侧栏是 26 列，最小 18、最大 36。额度行 27 列在默认宽度就已经超出内容区（还要扣缩进/边框）。截图渲染成 `cache... · no c...` 和 `5h 10... · 7d 8...`，而 `context 5%` 完整——说明截断发生在**每个 token 分到的宽度**上，不是整行从左往右切。若是整行截断，应接近 `cache 0.0% · n…` / `5h 100% 3h34m · …`。

## Herdr 合同（一手）

### 行是静态的，拼是固定的

[Configuration: Sidebar row layouts](https://herdr.dev/docs/configuration/#sidebar-row-layouts)：

- 展开的桌面侧栏把 `rows` 的每个内层数组画成一行。
- 相邻有值 token 默认用 `·` 分隔；`state_icon` 后面是一个空格。
- 缺值和它的分隔符一起消失；一行里所有 token 都没值时整行消失。
- 每个布局最多 16 行，每行最多 16 个 token。
- 折叠侧栏和 mobile 布局不使用这套 `rows`。

[Config reference](https://herdr.dev/docs/config-reference/) 列出的侧栏键只有宽度和行布局：`sidebar_width`（默认 26）、`sidebar_min_width`（18）、`sidebar_max_width`（36）、`row_gap`、`rows`、`rows_by_agent`。没有 wrap / overflow / ellipsis / separator 配置。token 内联样式只有 `{ token, fg, bold, dim }`。

本机 `herdr --default-config` 与上述一致。`herdr 0.8.0` 二进制依赖 `unicode-truncate`，没有侧栏 soft-wrap 字符串。

社区讨论 [herdr#1108 Title wrapping in Agents Panel](https://github.com/herdrdev/herdr/discussions/1108)（2026-07-07）：作者明确写现状是长标题在一行里被 `…` 截断，自己有一份最多 4 行、默认关闭的本地 wrapping。0.8.0 仍无对应配置项，维护者未把它收进稳定合同。

### 插件写不进换行

[CLI reference: report-metadata](https://herdr.dev/docs/cli-reference/#panes)：

- 存盘前会 trim、**去掉控制字符**、把 title / display-agent / state-label / **token 值截到 80 字符**。
- 规范化后的空 token 会清掉该 key。
- 一次最多 16 个 token 参数（本仓库 `MAX_METADATA_TOKENS`）。

因此 token 值里的 `\n` 进不了渲染；也不能靠超长字符串让 Herdr 折行。

插件只提供值，排版留在本地 `config.toml`。[Configuration](https://herdr.dev/docs/configuration/#sidebar-row-layouts) 原文：*Metadata reporters provide values only; styling stays in the local sidebar configuration.*

改 `rows` 需要写用户配置并 `herdr server reload-config`。那是安装期动作，不是每次拖侧栏都能做的事。`herdr server reload-config` 也不重读插件 manifest。

### 没有宽度输入

本机 `herdr api snapshot` 的 `snapshot` 键是：`agents`、`focused_*`、`layouts`、`panes`、`protocol`、`tabs`、`version`、`workspaces`。没有 `sidebar_width`。

`layouts[].area` 是**窗格区域**（本次 `x=36, width=281`）。`x=36` 可以当“侧栏大约占了左边多少列”的旁证，但：

- 官方 schema 把 `layout.updated` 定义成 `PaneLayoutSnapshot`（workspace / tab / panes / splits），不是侧栏面板。
- 插件可订阅的事件（本机二进制字符串）有 `layout.updated`、`pane.resize`、`pane.focused` 等，**没有** sidebar resize。
- `HERDR_PLUGIN_CONTEXT_JSON` 文档字段是 workspace / tab / pane / worktree / agent / 选中文本 / URL，没有侧栏宽度。

最便宜的核验（本次未做，因为不改变“没有换行 API”）：拖一次侧栏，看 `layout.updated` 会不会带着新的 `area.x` 进来。就算会，插件也只能改 token **文本**，不能改“这一行有几个 token”。改文本会走 `pane report-metadata`，token 变化时有 repaint 风险（见 `AGENTS.md`）。

## 现有拼接为什么不能直接“通用自适应”

仓库里的拼接是**内容折叠**，不是宽度折叠：

| 规则 | 位置 | 目的 |
| --- | --- | --- |
| cache、TTL、`no cached` 放同一行 | `append_cache_row` | 省一行；缺 TTL / 未过期则对应 token 消失 |
| 5h 为空时 7d 发到 `quota_week_inline_*` | `week_style_base` | 变成 `context · 7d`，省掉空的 limits 行 |
| 5h 有值时 7d 留在 limits 行 | 同上 | 避免 `context · 5h · 7d` |
| 5h 从不和 context 同行 | `append_quota_rows` 注释 | 有 5h 时 limits 单独一行 |

这套规则已经是“有值才占位”的通用折叠，Herdr 会处理空 token。它**不能**在 22 列装不下 `cache · ttl` 时把 ttl 挪到下一行，因为下一行是 config 里写死的，不是渲染时算出来的。

若做成“宽则拼、窄则拆”，插件必须在运行时改 `rows` 或改发布哪些 token。前者要写用户配置；后者要额外 packed/stacked 两套 token，会撞 16 token 上限，并且每次拖宽都可能 metadata 写入。两条路都不是通用、零成本的自适应。

对照：[herdr-agent-usage](https://github.com/senna-lang/herdr-agent-usage) 把 `$provider` 和 `$limit` 放同一行，但把 `$context` **单独一行**，并写明 Herdr 用 `·` 拼接且没有分隔符配置。他们也是静态行 + 空 token 消失，没有按宽度换行。

## 若要改本插件，还成立的做法

不增加 Herdr 能力的前提下，只有这些是合同内的：

1. **每字段一行（推荐，若目标是“宽窄都读得完”）**  
   cache、TTL/`no cached`、context、5h、7d 各一行。空行仍消失。`quota_week_inline_*` 不再需要：7d 永远在自己的行上，5h 空则 limits 只剩 7d。  
   验证：当前单 token 最长约 14 列（`5h 100% 3h07m`），小于 `sidebar_min_width = 18`。宽侧栏会多 1–2 行，不再截断。  
   代价：宽侧栏用户看到更高的 agent 卡片。

2. **安装期二选一**  
   packed（现状）或 stacked。不是动态的，但尊重“有人喜欢拉宽、有人喜欢窄”。

3. **不要做的**  
   - 根据 `layouts[].area.x` 猜测宽度再改 token：无稳定事件、会 metadata 写入、仍不能换行。  
   - 在 token 里塞换行或更长的 packed 字符串：控制字符会被剥掉，80 字符上限也帮不上。  
   - 同一行同时放 packed 和 stacked 两套 token：有值的会一起画出来。

真正的动态换行只能向 Herdr 要：行溢出时按 token 边界折到下一视觉行，或给 token 一个 wrap 样式。在那之前，本插件能通用处理的只有空字段折叠。

## 未做的核验

- 未在拖侧栏时抓 `layout.updated` 的 payload（不改变 API 结论）。
- 未反汇编 Herdr 行渲染函数确认每个 token 的宽度分配公式；截图已经足够排除“整行从左截断”。
- 未测 token 值含空格/`·` 时的二次截断细节。
