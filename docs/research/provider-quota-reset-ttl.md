# Grok/Agy quota reset 与 prompt-cache TTL

> Historical research, verified 2026-09-10. For current behavior, see the
> [README](../../README.md).

范围仅包括 provider 官方文档、官方 CLI 源码及本仓库实现。本文补充
[quota reset 调研](quota-reset-capability.md)和
[prompt-cache TTL 调研](oss-prompt-cache-ttl.md)，只记录 Grok/Agy 的新增结论。

## 结论

Quota reset 是额度窗口的结束时间，存入 `UsageWindow.resets_at`；prompt-cache
TTL 是缓存前缀的寿命，存入 `CacheUsage`。两者不能互相替代。

| Provider | quota reset | prompt-cache TTL | 插件行为 |
| --- | --- | --- | --- |
| Grok | `config.currentPeriod.end` | 无固定 TTL；缓存可能随时驱逐 | 解析 reset；缓存只显示计数 |
| Agy / Antigravity | `quota.<bucket>.reset_time`，可回退 `reset_in_seconds` | Status Line 未提供 expiry | 解析 reset；TTL 保持 unknown |
| Claude Code（对照） | `rate_limits.*.resets_at` | `prompt_cache.expires_at` | 两者分别解析 |
| Codex（对照） | `account/rateLimits/read` 的 `resetsAt` | 只有请求策略，没有实际 expiry | 解析 reset；不伪造 TTL |

## Grok

官方 Grok Build billing 合同提供：

```text
config.creditUsagePercent
config.currentPeriod.type
config.currentPeriod.start
config.currentPeriod.end
```

本仓库已从 billing 响应解析 weekly/monthly 类型及 RFC 3339 `end`。字段缺失或
格式未知时保持 unknown，不猜固定周期。登录凭据的 `expires_at` 仅用于选择有效
凭据，与额度或 prompt cache 无关。

Grok headless usage 只提供 cache read/write token 计数。xAI 官方说明缓存会因负载
或重启提前驱逐，因此不能从配额 reset、认证 expiry 或固定时长生成缓存倒计时。

## Agy / Antigravity

官方 Status Line 合同提供：

```text
quota.<bucket>.remaining_fraction
quota.<bucket>.reset_time
quota.<bucket>.reset_in_seconds  # optional
```

本仓库按活动模型选择 Gemini 或 third-party pool，优先解析绝对 `reset_time`，缺失时
才用快照时间加 `reset_in_seconds`。bucket 或 reset 字段缺失时保持 unknown，不混合
两个 pool，也不猜固定 5h/7d 周期。

Status Line 没有 prompt-cache expiry 字段。它的 context/cache 计数不能证明缓存
何时失效，因此 Agy 不生成缓存倒计时。

## 实现边界

- quota reset 只写入 `UsageWindow.resets_at`；prompt-cache expiry 只写入
  `CacheUsage`。
- 上游字段缺失时保留 unknown 或同一身份最后一次有效快照，不写零值或固定周期。
- Grok 继续使用 billing collector；Agy 继续使用 Status Line，不新增 UI 抓取、模型
  请求或未公开 endpoint。

## 一手来源

- Grok Build [`billing.rs`](https://github.com/xai-org/grok-build/blob/main/crates/codegen/xai-grok-shell/src/extensions/billing.rs)
- Grok Build [headless usage](https://github.com/xai-org/grok-build/blob/main/crates/codegen/xai-grok-pager/docs/user-guide/14-headless-mode.md)
- xAI [prompt caching behavior](https://docs.x.ai/developers/advanced-api-usage/prompt-caching/best-practices)
- Antigravity [Status Line fields](https://antigravity.google/docs/cli/statusline/)
- Claude Code [Status Line fields](https://code.claude.com/docs/en/statusline)
- Codex [rate-limit schema](https://github.com/openai/codex/blob/main/codex-rs/app-server-protocol/schema/json/v2/GetAccountRateLimitsResponse.json)
