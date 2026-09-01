# OMP/Claude 订阅额度与开源实现研究

> 研究日期：2026-09-01（Asia/Shanghai）  
> 目标：确认本仓库引用的 OMP（oh-my-pi/`omp`）如何表示额度，以及额度是否按“厂商”共享；再用其他开源项目交叉核对 Claude/Codex 的获取和展示方式。  
> 方法：研究步骤只读上游源码与项目文档；没有读取任何 `agent.db`、OAuth token 或凭据文件。

## 结论先行

1. **额度不是“厂商全局池”。** OMP 把每条限制绑定到 `provider`、`accountId`/`orgId`/`projectId`、模型/套餐和时间窗口；`scope.shared=true` 的含义是“该账号的这个窗口可由多个模型/请求共同消耗”，不是跨账号、跨客户端或跨厂商共享。模型与字段定义见 OMP [`usage.ts`](https://github.com/can1357/oh-my-pi/blob/65f79e76fcc89b96632fe86a598f314bd7cfc725/packages/ai/src/usage.ts#L1-L70)。
2. **Claude Code 与 OMP 可能显示同一池，但条件是同一 Anthropic OAuth 账号和同一组织/订阅工作区。** OMP 和多个开源工具都调用 Anthropic OAuth usage 接口；这是“同池”的合理推断，不是 Anthropic 官方对所有客户端的共享承诺。仅有相同邮箱也不够：OMP 明确按 `orgId` 区分同一邮箱下的 Team 与个人订阅。
3. **“能登录”不等于“能读订阅额度”。** OMP 的 Anthropic usage provider 只接受 OAuth credential；API key 可以用于推理，但不满足该 provider 的订阅 usage 查询条件。见 [`claude.ts`](https://github.com/can1357/oh-my-pi/blob/65f79e76fcc89b96632fe86a598f314bd7cfc725/packages/ai/src/usage/claude.ts#L606-L704) 与 [`anthropic.ts`](https://github.com/can1357/oh-my-pi/blob/65f79e76fcc89b96632fe86a598f314bd7cfc725/packages/ai/src/registry/anthropic.ts#L6-L25)。
4. **厂商适配不可避免。** Claude、OpenAI Codex、Grok 的 usage endpoint、认证头、响应 schema 和账户标识都不同；CodexBar、UsageBar 均为每个厂商写独立 fetcher/client，并为 OAuth、CLI、浏览器来源分别做 fallback。

## 1. OMP 如何表示和获取额度

### 1.1 统一模型是“多窗口 + 作用域”

`UsageReport` 包含一个 provider 的多个 `UsageLimit`。每条 limit 有窗口（例如 5 小时/7 天）、用量与上限/剩余量、状态，以及带账号/组织/模型/套餐信息的 `UsageScope`。这使得“共享”是单条 limit 的作用域属性，而非全局布尔开关：[OMP `usage.ts`](https://github.com/can1357/oh-my-pi/blob/65f79e76fcc89b96632fe86a598f314bd7cfc725/packages/ai/src/usage.ts#L31-L70)。报告还携带 `provider`、抓取时间、账户元数据和原始响应摘要：[同文件](https://github.com/can1357/oh-my-pi/blob/65f79e76fcc89b96632fe86a598f314bd7cfc725/packages/ai/src/usage.ts#L103-L118)。

### 1.2 Anthropic/Claude OAuth 路径

在 OMP 当前主分支（commit `65f79e76fcc89b96632fe86a598f314bd7cfc725`，包版本 18.0.11）中，Anthropic provider：

- 只在 `provider === "anthropic"` 且 credential 类型为 OAuth、存在 access token 时启用 usage fetch；调用 `${baseUrl}/usage` 并用 Bearer token。[`claude.ts`](https://github.com/can1357/oh-my-pi/blob/65f79e76fcc89b96632fe86a598f314bd7cfc725/packages/ai/src/usage/claude.ts#L606-L618)
- 解析 `five_hour`、`seven_day`、`seven_day_opus`、`seven_day_sonnet` 以及通用 `limits` 字段。普通 5h/7d 行标为 `scope.shared=true`；Opus/Sonnet 等模型专属周窗口不标 shared。[`claude.ts`](https://github.com/can1357/oh-my-pi/blob/65f79e76fcc89b96632fe86a598f314bd7cfc725/packages/ai/src/usage/claude.ts#L621-L704)，模型窗口的区分逻辑见[此处](https://github.com/can1357/oh-my-pi/blob/65f79e76fcc89b96632fe86a598f314bd7cfc725/packages/ai/src/usage/claude.ts#L531-L557)。
- 从 usage/profile 响应提取 account、email、organization，并写入报告 metadata；因此展示层可以把同一 provider 的不同订阅分开。[`claude.ts`](https://github.com/can1357/oh-my-pi/blob/65f79e76fcc89b96632fe86a598f314bd7cfc725/packages/ai/src/usage/claude.ts#L673-L696)

Anthropic OAuth 登录流程把组织视为订阅工作区，并说明一个邮箱可对应多个组织/订阅；登录时会保存 organization identity。[`registry/oauth/anthropic.ts`](https://github.com/can1357/oh-my-pi/blob/65f79e76fcc89b96632fe86a598f314bd7cfc725/packages/ai/src/registry/oauth/anthropic.ts#L96-L106)、[组织身份处理](https://github.com/can1357/oh-my-pi/blob/65f79e76fcc89b96632fe86a598f314bd7cfc725/packages/ai/src/registry/oauth/anthropic.ts#L178-L203)。

### 1.3 API key 与订阅 OAuth 不是同一额度域

Anthropic registry 同时列出 `ANTHROPIC_OAUTH_TOKEN` 和 `ANTHROPIC_API_KEY`，但 usage provider 的 `supports` 只接受 OAuth。[registry 定义](https://github.com/can1357/oh-my-pi/blob/65f79e76fcc89b96632fe86a598f314bd7cfc725/packages/ai/src/registry/anthropic.ts#L6-L25)、[usage supports](https://github.com/can1357/oh-my-pi/blob/65f79e76fcc89b96632fe86a598f314bd7cfc725/packages/ai/src/usage/claude.ts#L699-L704)。因此：

- Claude Pro/Max/Team 订阅的 5h/7d 窗口，应通过 Anthropic OAuth usage 查询；
- API key 的开发者 API 计费/速率限制是另一套域，不能因“同为 Anthropic”就推断会出现在 Pro/Max 窗口；
- OMP 显示 `No usage data` 时，首先区分“没有 OAuth 订阅凭据”和“OAuth endpoint 没返回 usage”，不要把它解释成额度为零。

### 1.4 OMP CLI、账户归属与缓存

`omp usage` 的设计是“列出每个已认证账户”，支持 `--provider`、`--json`、`--redact`，没有数据的账户也会列出：[usage 命令](https://github.com/can1357/oh-my-pi/blob/65f79e76fcc89b96632fe86a598f314bd7cfc725/packages/coding-agent/src/commands/usage.ts#L1-L55)。CLI JSON 中包含 `reports`、`accountsWithoutUsage`、禁用凭据和 capacity；展示逻辑按账户、窗口、模型 limit 渲染，并明确显示“no usage data”：[账户输出模型](https://github.com/can1357/oh-my-pi/blob/65f79e76fcc89b96632fe86a598f314bd7cfc725/packages/coding-agent/src/cli/usage-cli.ts#L41-L54)、[未报告账户归属](https://github.com/can1357/oh-my-pi/blob/65f79e76fcc89b96632fe86a598f314bd7cfc725/packages/coding-agent/src/cli/usage-cli.ts#L308-L380)、[文本 breakdown](https://github.com/can1357/oh-my-pi/blob/65f79e76fcc89b96632fe86a598f314bd7cfc725/packages/coding-agent/src/cli/usage-cli.ts#L630-L735)、[JSON/命令主流程](https://github.com/can1357/oh-my-pi/blob/65f79e76fcc89b96632fe86a598f314bd7cfc725/packages/coding-agent/src/cli/usage-cli.ts#L1028-L1204)。

OMP 的 AuthStorage 按 credential 获取报告，不把同一厂商的所有凭据合并成一个池；报告缓存有约 5 分钟 TTL、抖动和 last-good 保留，失败时进入冷却。[请求收集](https://github.com/can1357/oh-my-pi/blob/65f79e76fcc89b96632fe86a598f314bd7cfc725/packages/ai/src/auth-storage.ts#L3627-L3710)、[身份去重/合并](https://github.com/can1357/oh-my-pi/blob/65f79e76fcc89b96632fe86a598f314bd7cfc725/packages/ai/src/auth-storage.ts#L3712-L3840)、[缓存与失败保留](https://github.com/can1357/oh-my-pi/blob/65f79e76fcc89b96632fe86a598f314bd7cfc725/packages/ai/src/auth-storage.ts#L3284-L3443)。源码还特别注明：同一邮箱的多个 Anthropic/OpenAI 组织订阅不能跨 org 合并。[去重注释](https://github.com/can1357/oh-my-pi/blob/65f79e76fcc89b96632fe86a598f314bd7cfc725/packages/ai/src/auth-storage.ts#L3743-L3759)

### 1.5 OpenAI Codex 和 xAI 对照

- OMP 的 `openai-codex` usage provider 同样只接受 OAuth，调用 ChatGPT backend 的 `/wham/usage`，携带 Bearer 与 `ChatGPT-Account-Id`，把主/次窗口和 meter states 映射为 account-scoped limits。[provider 定义与支持条件](https://github.com/can1357/oh-my-pi/blob/65f79e76fcc89b96632fe86a598f314bd7cfc725/packages/ai/src/usage/openai-codex.ts#L396-L442)、[limit 映射](https://github.com/can1357/oh-my-pi/blob/65f79e76fcc89b96632fe86a598f314bd7cfc725/packages/ai/src/usage/openai-codex.ts#L447-L560)。
- `xai-oauth` 代表 SuperGrok 订阅 usage，API key 是另一条 paid API 路径；周/月窗口按产品返回，不能把 xAI API 余额和 SuperGrok 订阅合并。[xAI provider 说明](https://github.com/can1357/oh-my-pi/blob/65f79e76fcc89b96632fe86a598f314bd7cfc725/packages/ai/src/usage/xai-oauth.ts#L1-L10)、[窗口和 shared scope](https://github.com/can1357/oh-my-pi/blob/65f79e76fcc89b96632fe86a598f314bd7cfc725/packages/ai/src/usage/xai-oauth.ts#L240-L307)。

## 2. 其他开源项目的交叉核对

### CodexBar（commit `8a732e743564abdb68ab3bee9332153ef88597a4`）

CodexBar 的 Claude OAuth fetcher 直接 GET `https://api.anthropic.com/api/oauth/usage`，带 Bearer、`anthropic-beta` 和 Claude Code User-Agent；处理 200/401/429，并解析 5h、7d、模型窗口。[源码](https://github.com/steipete/CodexBar/blob/8a732e743564abdb68ab3bee9332153ef88597a4/Sources/CodexBarCore/Providers/Claude/ClaudeOAuth/ClaudeOAuthUsageFetcher.swift#L60-L127)。其文档要求 OAuth 的 `user:profile` scope，并说明没有 OAuth 时可从 CLI/网页来源回退；窗口字段仍来自 Anthropic usage 响应。[Claude 文档](https://github.com/steipete/CodexBar/blob/8a732e743564abdb68ab3bee9332153ef88597a4/docs/claude.md#L63-L102)

CodexBar 的 Codex OAuth fetcher 是另一套 ChatGPT backend 实现：`/wham/usage`、Bearer、`ChatGPT-Account-Id` 和独立响应模型。[源码](https://github.com/steipete/CodexBar/blob/8a732e743564abdb68ab3bee9332153ef88597a4/Sources/CodexBarCore/Providers/Codex/CodexOAuth/CodexOAuthUsageFetcher.swift#L391-L456)。这两个 fetcher 的并存说明没有可跨厂商复用的“通用额度 API”。

### UsageBar（commit `f42bd4c7e2aebfa1ee553430271b691ce481551e`）

UsageBar 在菜单栏显示 Claude/Codex 的 5h/7d 条、重置时间和模型细分；Claude 通过 OAuth，Codex 读取本地 CLI OAuth。[README 功能与登录](https://github.com/methol-dev/usage-bar/blob/f42bd4c7e2aebfa1ee553430271b691ce481551e/README.md#L24-L34)、[凭据来源](https://github.com/methol-dev/usage-bar/blob/f42bd4c7e2aebfa1ee553430271b691ce481551e/README.md#L65-L83)。

它的 Claude CLI client 与 OMP/CodexBar 使用同一 Anthropic OAuth usage endpoint，并处理 Keychain、401 重读、429 backoff 和 stale 数据：[UsageService.swift](https://github.com/methol-dev/usage-bar/blob/f42bd4c7e2aebfa1ee553430271b691ce481551e/macos/Sources/UsageBar/Providers/Claude/UsageService.swift#L51-L55)、[请求与错误处理](https://github.com/methol-dev/usage-bar/blob/f42bd4c7e2aebfa1ee553430271b691ce481551e/macos/Sources/UsageBar/Providers/Claude/UsageService.swift#L150-L238)。它还把 Claude 网页来源单独建模（浏览器扩展写入文件），并从 `claude.ai/api/organizations/{id}/usage` 与 ChatGPT `/wham/usage` 获取数据：[扩展来源](https://github.com/methol-dev/usage-bar/blob/f42bd4c7e2aebfa1ee553430271b691ce481551e/extension/background.js#L288-L357)。

这些项目都遵循同一事实：**共享的是服务端账户/组织窗口；客户端只是不同的读取器。** 但读取器必须针对每个厂商的认证和响应协议适配，且可能因 OAuth scope、限流、网页/CLI 登录状态而暂时没有数据。

## 3. 对当前“Claude 有额度耗尽、OMP 只有登录”的最小核验

本机在 omp `18.0.11` 上已经复现：过滤 Anthropic 后 `reports` 为空，账号出现在
`accountsWithoutUsage` 且 credential type 为 OAuth；同一次调用对应的脱敏日志是
`Claude usage fetch failed`、HTTP 429。OMP 对 429 明确不做同轮重试，因为这个
usage endpoint 按来源 IP 限流；调用方应进入冷却并保留 last-good，而不是继续轮询。
见 OMP [`claude.ts`](https://github.com/can1357/oh-my-pi/blob/b8ce33a58911c26bed1d84f0db9a5e2e727c49a2/packages/ai/src/usage/claude.ts#L232-L327)。

因此当前空白的直接原因是 **OMP 没拿到 quota report**，不是 Herdr 解析漏掉了
5h/7d 字段。这个 429 也不能解释为“额度为 0”：它是读取用量端点失败，而不是一个
可展示的 quota snapshot。

不读取凭据内容即可运行：

```sh
omp usage --json --provider anthropic
omp usage --json --redact --provider anthropic
```

按 JSON 结果分支判断：

| 结果 | 更可能的含义 | 下一步 |
| --- | --- | --- |
| `reports` 有 `five_hour`/`seven_day`，metadata 含 account/org | OMP 已读到订阅窗口；若与 Claude Code 的账号/org 相同，通常应是同一服务端池（推断） | 对比账号/org，等待服务端刷新；不要按邮箱单独合并 |
| `accountsWithoutUsage`，credential type 为 API key | 只有 API key，无法走 OMP 的 Anthropic 订阅 usage provider | 用 OMP 的 Anthropic OAuth 登录，再重跑命令 |
| OAuth credential 但 `No usage data`/空 report | endpoint 返回 401/403/429、token 过期/禁用、scope 不足，或响应没有可解析窗口 | 看 `--redact` 的账户归属和错误状态；不要把空结果当“额度为 0” |
| OMP account/org 与 Claude Code 不同 | 两个订阅工作区，额度不应合并 | 在同一 org 重新登录/选择正确账户 |

仅凭“Claude Code 显示耗尽、OMP 可以登录”无法确定是哪一分支；必须先看上述无凭据输出。Claude Code 本身是闭源客户端，因此本研究不能从公开源码证明 Anthropic 对所有客户端的跨产品共享规则；“同 OAuth 账号/org 调同一 usage endpoint ⇒ 大概率同池”应标为推断。

## 4. 对 herdr-agent-quota 的含义

- OMP 适配应把 provider 具体传给 `omp usage --json --provider <id>`，而不是请求“所有 provider”后猜测厂商池。
- 归属键至少保留 provider + accountId/orgId（必要时 projectId）；只在 OMP 报告声明的同一 scope 内展示 `shared` 窗口。
- “无 usage 数据”应保留上次已知 topic/数字并标记未知，不能清零后误报耗尽；这也符合 OMP 的 last-good 设计。
- 本次实现据此补了两个边界：首次得到可归属的 `accountsWithoutUsage` 时显示
  `N/A`；同账号已有 last-good 时继续显示旧值。没有快照时也执行 60 秒去抖，避免
  每个 pane 事件再次启动 `omp usage`、加重 429。
