# herdr-agent-quota

在 Herdr agent 侧栏显示按凭据范围隔离的 AI 额度与上下文使用情况。

[![CI](https://github.com/levi-qiao/herdr-agent-quota/actions/workflows/ci.yml/badge.svg)](https://github.com/levi-qiao/herdr-agent-quota/actions/workflows/ci.yml)
[![Rust](https://img.shields.io/badge/built%20with-Rust-dea584?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

English: [README.md](README.md)

```text
● Owner · Claude/Sonnet
  fix the release check
  cache 99.6% · ttl≈58m
  context 23%
  5h 100% 3h07m · 7d 31% 2d3h
```

这是默认的拼接布局。`--sidebar-layout stacked` 把供应商、模型和每个额度字段
单独一行，窄侧栏时不会把 `tab · Claude/Sonnet`、`cache · ttl` 或 `5h · 7d`
截成省略号：

```text
● Owner
  Claude
  Sonnet
  fix the release check
  cache 99.6%
  ttl≈58m
  context 23%
  5h 100% 3h07m
  7d 31% 2d3h
```

<table>
<tr>
<th>packed（默认）</th>
<th>stacked</th>
</tr>
<tr>
<td valign="top"><img src="docs/screenshots/sidebar-packed.png" alt="拼接布局" width="284"></td>
<td valign="top"><img src="docs/screenshots/sidebar-stacked.png" alt="分行布局" width="177"></td>
</tr>
</table>

插件只展示能够归属到当前 pane 精确 session 和凭据范围的数据。空字段会自动折叠；刷新失败保留
同一账户最后一次成功额度；已确认的 PAYG session 会清掉旧订阅额度。

## 安装

要求：Herdr 0.8.0+、Rust 1.95+、macOS 或 Linux，以及至少一个受支持的 agent CLI。

```sh
./install.sh
```

安装后，已经运行的 agent pane 需要重启一次。

### 必需的 Herdr integration

Herdr 必须先知道 pane 的 session，插件才能安全归属额度。检查内置 integration：

```sh
herdr integration status
```

安装缺失项，然后重启对应 pane：

```sh
herdr integration install opencode
herdr integration install pi
```

缺少 integration 时，Herdr 可能能识别 agent，但拿不到 session，因此侧栏保持空白。
`configure --check` 和 `configure --apply` 也会打印准确的修复命令。

### 只安装需要的 agent

```sh
herdr-agent-quota configure --apply --agent claude,codex,pi
```

可选值：`all`、`claude`、`codex`、`grok`、`agy`、`opencode`、`pi`。
安装器不会替换用户自己维护的侧栏 rows 或 statusLine hook。

### 不重装也能改设置

打开 **Agent quota settings** 插件 pane。可以改百分比口径、侧栏布局、行间距和
watcher 间隔；按 `a` 应用时，它跑的就是 "Install / repair" 那条 `configure
--apply`，然后 reload Herdr 配置。

```
> * Percentages     <    used    >  how much quota is spent
    Sidebar layout  <  stacked   >  every field on its own row
    Row gap         <     1      >  one blank row between panes
    Watch interval  <     1m     >  polled while an agent is working

  ↑↓ field   ←→ value   a apply   q quit
```

`*` 表示改了还没应用。agent 选择只展示、不可编辑：去掉一个 agent 必须真正卸载它的
collector，仍然走 `./install.sh --agent` 和 `./uninstall.sh --agent`。

手动刷新或卸载：

```sh
herdr plugin action invoke herdr-agent-quota.refresh
./uninstall.sh
```

## 覆盖范围

| Harness | 订阅额度 | 精确 session 信息 | 归属规则 |
| --- | --- | --- | --- |
| Claude Code | 5h + 7d | model、context、cache、statusLine `prompt_cache` 过期时间 | Claude statusLine session |
| OpenAI Codex | 5h + 7d | model、context、cache、估算 cache TTL、session 摘要 | 规范 ChatGPT 登录；API key 不冒充订阅额度 |
| Grok CLI | 7d；月付方案为 30d | model、context、cache | 规范 Grok 登录 |
| Agy / Antigravity | 5h + 7d | model、context、cache | Agy statusLine session 与当前模型池 |
| OpenCode | OpenCode Go 5h + 7d；dashboard 面板含 30d | 精确本地 session 的 model 与 context | 仅 `opencode-go` 和其匹配 key；其他 backend 不借用 Go 凭据 |
| Pi | 能安全匹配时复用现有 Codex 额度 | model、context、cache；Anthropic 已记录 TTL、Codex 估算 TTL | 仅 account id 与规范 Codex 相同的 `openai-codex` OAuth |

额度显示剩余百分比和重置时间。侧边栏有一个 5h 窗口和一个长周期槽位：长周期槽位放
周额度，若该方案没有周额度则放月额度。周期标签跟着数值走（`7d 69% 6d0h`、
`30d 70% 17d8h`），所以月额度绝不会被显示成周额度；两者同时存在时永远是周额度占据
该槽位。dashboard 面板会列出该方案的全部窗口，30d 与 7d 并列。context 显示已用百分
比；cache 在上游 session 格式可支持时显示命中率。有记录的过期时间优先：Claude Code 的
`prompt_cache.expires_at`（v2.1.251+）和 Pi/Anthropic 的 `cacheWrite1h`。
Codex 本地既没有 TTL 也没有 expiry——rollout JSONL 只有 cache token 计数和请求
时间戳——所以按 Responses API 文档中默认且当前唯一支持的
`prompt_cache_options.ttl`（30 分钟）估算，锚点是最后一次已记录的请求。显示同样是
`ttl≈`，但它是估算：前缀变化、compaction、system/tool 定义变化都可能让命中率在计时
结束前先掉下去。Grok、Agy、OpenCode 和其他 Pi backend 的本地合同既没有过期时间也没有
可依据的 TTL，因此只保留命中率，不猜 TTL。

Pi 只读取 Herdr 报告的那个绝对 JSONL 路径，来源为 `~/.pi/agent` 或
`PI_CODING_AGENT_DIR`，不会扫描所有 session。API key session 确认为 PAYG 并清除旧额度；
缺失、损坏、不支持或账户不匹配的证据保持 Indeterminate，不动已有订阅额度。

OpenCode 只在 `opencode.db` 精确查询一个 session id，并读取 `auth.json` 中相同 provider
的凭据类型。即使当前 backend 没有支持的订阅 collector，也会从该 session 和有界的本地
model cache 展示准确 model/context。

### OpenCode Go 仍需要真实订阅验证

维护者**没有 OpenCode Go 订阅**。本地 session、凭据、请求、错误和 fail-closed 路径已在
OpenCode 1.18.20 上验证；成功响应结构来自
[CodexBar](https://github.com/steipete/CodexBar)，证据记录在
[`docs/research/opencode-go-usage.md`](docs/research/opencode-go-usage.md)，不是本仓库实际观察到的
成功响应。

未知或损坏响应不会发布数据，401/403 也不会变成 0%。如果你有 OpenCode Go，欢迎提供脱敏
响应或确认数字是否一致；issue 和 PR 都非常欢迎。

## 侧栏行为

- 两种布局：`packed` 把 tab 和 provider/model、cache 和 TTL、5h 和 7d 拼在同一行；
  没有 5h 时 7d 仍会折到 context。`stacked` 让供应商、模型、cache、TTL、context、
  5h、7d 各占一行。两种布局下空 token 都会折叠。
- token 全空的行会折叠。插件自有布局默认 `row_gap = 1`（pane 之间一行空白）。
  `--row-gap 0` 贴紧。Herdr 只接受整行；用户自己写的 `row_gap` 不会被改。
- 百分比默认显示**剩余**额度。`--quota-percent used` 把 5h/7d/30d 的数字换成
  **已用**额度；侧栏 token 宽度不变，颜色依然按剩余量计算，红色永远表示快用完。
- provider/model、topic、cache/TTL、context 和限额只在有可靠数据时显示。
- tab 用主文字色（`#eceef2`）。prompt 用正文色（`#c8cdd6`）。cache、TTL、
  context 用 metadata 灰（`#969eae`）。品牌色只给供应商；模型用同色系的
  dim。选中态只许改卡片背景，不许改供应商 hue。
  每个 5h/7d 窗口是一个紧凑 token（`5h 0% 1h18m`），空格分隔。Herdr 会把
  同行 token 用 ` · ` 拼起来，所以窗口不能拆成标签/百分比/倒计时三块。
  剩余百分比颜色：≥50% 绿，20–49% 琥珀，<20% 红。前缀缓存过期显示 `no cached`，
  同样是琥珀，但用独立 token：它是正常状态，不会和「额度完全读不到」的错误共用
  同一个 token。
  packed 布局里两个窗口之间仍会有一个 ` · `。
- event 只用 `--source visible` 读取事件点名的 pane；启动、focus、refresh 和 watcher 不读 pane。
- 只有 token 真正变化时才写 metadata，并始终遵守 Herdr 的 16-token 上限。

默认 watcher 间隔为 60 秒，安装时可调整。侧栏布局默认 `packed`，间距默认 `1`，
百分比默认 `remaining`，选择会写入插件状态，之后的 **Install / repair** 会沿用：

```sh
./install.sh --watch-interval-seconds 300
./install.sh --sidebar-layout stacked
./install.sh --row-gap 0
./install.sh --quota-percent used
herdr-agent-quota configure --apply --sidebar-layout packed --row-gap 1
herdr-agent-quota configure --apply --quota-percent remaining
```

## 数据与隐私

- 凭据只从 agent 自己的 store 读入内存，不记录、不缓存、不刷新，也不放进命令参数。
- 本地 session 读取精确且有界。OpenCode 使用只读 SQLite 查询；Pi 和其他 JSONL reader
  都限制总字节数与单行大小。
- 不抓浏览器 cookie 或钥匙串，不发送 prompt，也不发起模型请求。
- snapshot 只含脱敏百分比和重置时间，保存在 Herdr plugin state 目录。
- 网络请求仅访问受支持 CLI 的额度 endpoint；失败时保留最后一次成功值。

## 常见问题

| 现象 | 处理 |
| --- | --- |
| OpenCode 或 Pi 全空 | 运行 `herdr integration status`，安装缺失 integration，然后重启该 pane。 |
| 侧栏 row 不出现 | 运行 **Install / repair agent quota** 和 `herdr server reload-config`。 |
| Claude 或 Agy 显示 `N/A` | 发送一轮消息，让 statusLine 产生 snapshot。 |
| Pi model/context 旧 | 发送一轮 Pi 消息；成功 assistant message 后才确认模型切换。 |
| OpenCode 有 model/context 但无额度 | Zen、PAYG、OAuth 或缺少/未验证 Go key 时属于正常行为。 |
| 刷新失败 | 插件会保留同一账户最后一次成功值。 |
| Codex 的 5h 还是上一个 ChatGPT 账号的 | `codex login` 会重写 `~/.codex/auth.json`；下一次刷新应在新账号只有 7d 时立刻隐藏 5h。若侧栏仍旧，发一轮对话即可。 |
| `cache · ttl` 或 `5h · 7d` 被截成省略号 | Herdr 不会按侧栏宽度换行。用 `./install.sh --sidebar-layout stacked` 重装。 |

## 开发检查

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features --locked
cargo build --release --locked
```

解析器和 fixture 规则见 [`CONTRIBUTING.md`](CONTRIBUTING.md)，安全问题见
[`SECURITY.md`](SECURITY.md)，版本变化见 [`CHANGELOG.md`](CHANGELOG.md)。

## 许可证

MIT。本项目与 Herdr、OpenAI、Anthropic、xAI、Google 或 OpenCode 无隶属关系。
