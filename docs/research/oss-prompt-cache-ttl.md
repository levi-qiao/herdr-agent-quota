# 高星开源项目如何判断 prompt cache 过期时间

研究日期：2026-08-30（Asia/Shanghai）  
范围：按 GitHub 星数看用量/statusline/HUD 项目，以及各家官方 prompt-caching 合同。  
问题：别人是怎么得到 `ttl≈58m` 这种倒计时的？有没有 Codex / Grok / Agy 可以照抄的做法？

## 结论先行

1. **真正做“cache 还剩几分钟”的高星项目，几乎全是 Claude 专用。** 它们用同一套本地估算：从 transcript 尾部找最近一次带 cache 活动的 assistant 时间戳，加上 5 分钟或 1 小时的滑动窗口。没有项目能从 Codex / Grok / Gemini 的本地 usage 里读出 entry 级 `expires_at`。
2. **TTL 时长从哪来，分四档，诚实度递减：**
   1. 官方给了绝对过期时间（Claude Code `prompt_cache.expires_at`，v2.1.251+）
   2. 官方给了写入桶（`ephemeral_5m` / `ephemeral_1h`），本地用“最后一次命中 + 桶时长”
   3. 用户或配置写死 5m / 1h
   4. 按厂商文档猜一个默认值（OpenCode 插件表：Anthropic 5m、OpenAI 5m、Google 1h）
3. **星数最高的用量工具并不显示 TTL 倒计时。** CodexBar（~20.7k）和 ccusage（~18.2k）只统计 cache 读写 token / 成本。它们的 “TTL” 是自己磁盘缓存的刷新间隔（60s、1h），不是模型 prompt cache 的剩余寿命。
4. **对本插件：** Claude 已改为读官方 `prompt_cache.expires_at`（Claude Code v2.1.251+），不再从 transcript 桶估算。Codex / Grok / Agy 没有对等字段，高星项目也没发明一个；硬猜 1 小时会和 OpenAI「5–10 分钟、最长 1 小时、也可能更久」以及 xAI「随时驱逐」的合同打架。

## 高星项目对照

| 项目 | 约星数 | 做什么 | prompt cache 过期怎么判 | 证据 |
| --- | ---: | --- | --- | --- |
| [CodexBar](https://github.com/steipete/CodexBar) | 20.7k | 菜单栏额度 / 本地 cost | **不显示** prompt-cache 倒计时。内部 `tokenFetchTTL = 1h`、扫描 `refreshMinIntervalSeconds = 60` 是自己的数据刷新间隔 | [README](https://github.com/steipete/CodexBar/blob/main/README.md)、[#2103](https://github.com/steipete/CodexBar/issues/2103)、[#411](https://github.com/steipete/CodexBar/issues/411) |
| [ccusage](https://github.com/ccusage/ccusage) | 18.2k | 本地 token / 成本报表 | **不显示倒计时。** 把 `cache_creation` / `cache_read` 计入成本；1h 写入应按 2× 而不是 5m 的 1.25× 计价 | [ccusage.com](https://ccusage.com/guide/)、[#899](https://github.com/ryoppippi/ccusage/issues/899) |
| [ccstatusline](https://github.com/sirmalloc/ccstatusline) | 12.6k | Claude statusLine | transcript 尾部最近一次有 cache 活动的 assistant 时间戳 + **用户可切换的 5m/1h**（默认 5m）。文档写明 transcript **没有真正的 expiry**，倒计时是 best effort | [USAGE.md Cache Timer](https://github.com/sirmalloc/ccstatusline/blob/main/docs/USAGE.md)、[DEVELOPMENT.md](https://github.com/sirmalloc/ccstatusline/blob/main/docs/DEVELOPMENT.md) |
| [claude-code-usage-bar](https://github.com/leeguooooo/claude-code-usage-bar) / `claude-statusbar` | 0.3k+ | Claude statusLine | 反向读 transcript ≤320 KiB；`ephemeral_1h > 0` → 3600s，`ephemeral_5m > 0` → 300s，否则回退 300s；`remaining = TTL − (now − last assistant ts)` | [docs/cache-countdown.md](https://github.com/leeguooooo/claude-code-usage-bar/blob/main/docs/cache-countdown.md) |
| [ilia-pluzhnikov/claude-code-statusline](https://github.com/ilia-pluzhnikov/claude-code-statusline) | 低星 | Claude statusLine | 同：stdin 计数 + 约 16 KiB transcript 尾 + 桶 + timestamp。承认 stdin 没有 TTL | 本仓库既有调研 [`cache-observability-open-source.md`](cache-observability-open-source.md) |
| OpenCode `opencode-cache-hit` / `opencode-cache-timer` | 插件级 | OpenCode 面板 | **按 provider 写死默认 TTL**：Anthropic 5m、OpenAI 5m、Google 1h、DeepSeek 2h、xAI 5m。显示的是「已经活了多久」相对这个猜测值，不是 entry expiry | [npm opencode-cache-hit](https://www.npmjs.com/package/opencode-cache-hit) |
| [GandzyTM/claude-cache-statusline](https://github.com/GandzyTM/claude-cache-statusline) | 0 | Claude statusLine | 假定滑动 1h；用 transcript **字节增长**当 last-activity（不用 mtime） | 项目 README |
| [duyet/codex-claude-plugins](https://github.com/duyet/codex-claude-plugins) | — | Claude statusLine | Anthropic 才显示 TTL；`cache_ttl` 配置 `"5m"` 或 `"1h"`，新请求重置时钟 | [statusline README](https://github.com/duyet/codex-claude-plugins/blob/master/statusline/README.md) |

星数只作筛选权重。低星项目里只要算法被高星复用（transcript + 桶），仍记为同一族。

## Claude：大家实际在算什么

### 共同公式

Anthropic 官方：命中会刷新 TTL，TTL 从请求开始计时，只有 `5m` 和 `1h` 两档。

[Claude Code prompt caching：Cache lifetime](https://code.claude.com/docs/en/prompt-caching#cache-lifetime)  
[Anthropic API prompt caching](https://platform.claude.com/docs/en/build-with-claude/prompt-caching)

开源倒计时几乎都是：

```text
anchor  = 最近一次带 cache_read 或 cache_creation 的 assistant 时间戳
ttl     = 3600  若 ephemeral_1h_input_tokens > 0
        = 300   若 ephemeral_5m_input_tokens > 0
        = 用户配置或 300  （无桶时）
remaining = max(0, ttl − (now − anchor))
```

关键选择：

| 选择 | 高星怎么做 | 为什么 |
| --- | --- | --- |
| 锚点 | assistant 记录，不是 user 消息，不是文件 mtime | mtime 会被非 turn 的写入碰到；user 消息不刷新服务端 cache |
| 读多少 | 尾部 16–320 KiB，或增量 offset | 全量 JSONL 会把 statusLine CPU 打满（ccstatusline #137） |
| 无桶时 | ccstatusline 默认 5m；usage-bar 回退 5m；本插件 **不显示** | 回退会把未知策略伪装成 5m |
| 滑动窗口 | 每一轮新 assistant 重置 | 与官方 “hit refreshes TTL” 一致 |

这正是本插件 `src/providers/statusline.rs` 现在对 Claude 做的事（桶 + timestamp，无桶则隐藏）。

### 2026 年桶会变，所以不能写死 1h

Claude Code 客户端曾经整段时间只写 `ephemeral_1h`，后又整段只写 `ephemeral_5m`（约 2.1.218 起，见 [anthropics/claude-code#84253](https://github.com/anthropics/claude-code/issues/84253)）。订阅超额会降到 5m。所以 **看计划名猜 TTL 会错**；高星后来都改成读桶。

### 新合同：官方已经给了 `expires_at`

Claude Code **v2.1.251+** 的 statusLine stdin 增加 `prompt_cache`（[官方 statusLine：prompt cache fields](https://code.claude.com/docs/en/statusline#prompt-cache-fields)）：

| 字段 | 含义 |
| --- | --- |
| `warm` | 前缀是否仍在 TTL 内 |
| `ttl` | `"5m"` 或 `"1h"` |
| `expires_at` | Unix 秒，前缀变冷的时刻；无 cache token 时为 `null` |
| `hit_ratio` | session 累计命中 |

Claude Code 还会在 warm cache 到达 `expires_at` 时主动再跑一次 statusLine。这是目前唯一的 **entry 级绝对过期时间**。

截至本调研：ccstatusline 文档仍写 “transcript 没有真正 expiry，倒计时 best effort”；尚未看到它改读 `prompt_cache.expires_at`。本插件也还没读这个对象。

## Codex / OpenAI：高星项目不倒计时

官方合同（[OpenAI Prompt caching：Cache lifetime](https://developers.openai.com/api/docs/guides/prompt-caching#cache-lifetime)）：

| 模型 | 控制项 | 寿命 |
| --- | --- | --- |
| GPT-5.6+ | `prompt_cache_options.ttl`，目前只有 `"30m"` | **至少** 30 分钟，服务端可以留更久 |
| 更早 | `prompt_cache_retention`：`in_memory` 或 `24h` | in_memory 空闲约 5–10 分钟、最长约 1 小时；24h 约 30 分钟起、最长 24 小时 |

Codex app-server 的 `thread/tokenUsage/updated` 只有 `cachedInputTokens` / `cacheWriteInputTokens`，没有 expiry。CodexBar、ccusage 的 Codex 路径都只做 token/cost，不做 TTL HUD。

把 “1 小时” 当成 Codex 倒计时，会同时错过 30m 默认、5–10 分钟 in_memory、以及 “可能留更久”。没有高星项目这么做。

## Grok / Gemini / OpenCode

- **xAI**：缓存因内存压力和路由随时淘汰，没有公开 TTL。[How it works](https://docs.x.ai/developers/advanced-api-usage/prompt-caching/how-it-works) 原文：*not 100% guaranteed. Cache entries can be evicted due to memory pressure*。
- **Gemini implicit cache**：只报 `usage.total_cached_tokens`，没有 TTL。[Gemini context caching](https://ai.google.dev/gemini-api/docs/caching) 只建议 “短时间内发相似前缀”。显式 `CachedContent.expireTime` 是另一套资源 API，Antigravity statusLine 不暴露。
- **OpenCode 插件** 用文档默认值表做颜色带（绿/黄/红），那是 **elapsed vs 猜测 TTL**，不是过期时刻。

## 对本插件的含义

| Provider | 高星共识 | 本插件现状 | 可做的下一步 |
| --- | --- | --- | --- |
| Claude | transcript 桶估算；新版可直接读 `expires_at` | 已读 `prompt_cache.expires_at`，不再猜桶 | 保持；旧 Claude 不兼容 |
| Pi / Anthropic | 同 Claude 桶 | 已用 `cacheWrite1h` | 保持 |
| Codex | 不显示 TTL | 不显示 | 保持空白，除非 app-server 出现 expiry 字段 |
| Grok | 官方说随时驱逐 | 不显示 | 保持空白 |
| Agy / Gemini | implicit cache 无 TTL | 不显示 | 保持空白 |
| OpenCode | 插件猜表 | 不显示 | 不要抄硬编码表 |

不要把 quota `resets_at`、文件 mtime、或 “OpenAI 最长一小时” 当成 cache expiry。高星项目里，做对的都没这么干。
