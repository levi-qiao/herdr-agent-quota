# OMP Antigravity 的 `1d` 配额窗口：订阅与时间窗不是一回事

> 研究日期：2026-09-01（Asia/Shanghai）  
> 目标：解释 `omp usage --json` 对 `google-antigravity` 输出
> `window.durationMs = 86400000`（`1d`）的来源，并判断它是否意味着没有付费订阅。  
> 范围：只读 OMP 固定版本 `v18.0.11`
> （commit [`b8ce33a58911c26bed1d84f0db9a5e2e727c49a2`](https://github.com/can1357/oh-my-pi/commit/b8ce33a58911c26bed1d84f0db9a5e2e727c49a2)）、
> OMP 当前 `main`（commit [`65f79e76fcc89b96632fe86a598f314bd7cfc725`](https://github.com/can1357/oh-my-pi/commit/65f79e76fcc89b96632fe86a598f314bd7cfc725)）以及 Google 第一方源码/文档；没有读取任何 `agent.db` 或凭据。

## 结论

1. **已登录订阅与 `1d` 并不矛盾。** Google 的第一方 Gemini CLI 文档把 Google AI Pro、Google AI Ultra、Code Assist Standard/Enterprise 都列为固定价格（paid tier）订阅，同时明确给出“每用户每天”的请求上限。订阅决定可用的 tier/额度大小，**不决定一定是 5 小时或 7 天窗口**。[Google Gemini CLI quotas and pricing](https://github.com/google-gemini/gemini-cli/blob/0bd1d439751478771c45d3d0895a6a9760554bf4/docs/resources/quota-and-pricing.md#L13-L35) [Google Gemini CLI paid tiers](https://github.com/google-gemini/gemini-cli/blob/0bd1d439751478771c45d3d0895a6a9760554bf4/docs/resources/quota-and-pricing.md#L74-L124)
2. **OMP 的 `1d` 不是简单把“订阅”判断成免费。** Antigravity provider 调用 `https://daily-cloudcode-pa.googleapis.com/v1internal:fetchAvailableModels`，读取每个模型的 `quotaInfo`（以及可选的 `dailyQuotaInfo`、`weeklyQuotaInfo`、`quotaInfoByWindow`）中的 `remainingFraction`、`resetTime`、`windowId`、`windowLabel` 等字段。[OMP v18.0.11 `google-antigravity.ts`](https://github.com/can1357/oh-my-pi/blob/b8ce33a58911c26bed1d84f0db9a5e2e727c49a2/packages/ai/src/usage/google-antigravity.ts#L19-L49)
3. **窗口来源有两种：服务端明确标记，或 OMP 推断。** 若 `windowId`/`windowLabel` 含 `day`、`daily`、`24h`，OMP 标成 daily；若含 `week`、`7d`，标成 weekly。若通用 `quotaInfo` 没有窗口字段，OMP 按 `resetTime - now` 推断：大于 24 小时才算 weekly，否则算 daily。[OMP v18.0.11 分类与推断](https://github.com/can1357/oh-my-pi/blob/b8ce33a58911c26bed1d84f0db9a5e2e727c49a2/packages/ai/src/usage/google-antigravity.ts#L51-L113)
4. **`durationMs: 86400000` 是 OMP 的固定常量，不是由实际倒计时计算出来。** OMP 的 `DAY_MS = 24 * HOUR_MS`、`WEEK_MS = 7 * DAY_MS`；推断为 daily 后直接写入 `DAY_MS`，解析出的 `resetTime` 另存为绝对 `resetsAt`。[OMP `shared.ts`](https://github.com/can1357/oh-my-pi/blob/b8ce33a58911c26bed1d84f0db9a5e2e727c49a2/packages/ai/src/usage/shared.ts#L3-L21) [OMP 窗口输出](https://github.com/can1357/oh-my-pi/blob/b8ce33a58911c26bed1d84f0db9a5e2e727c49a2/packages/ai/src/usage/google-antigravity.ts#L129-L153)
5. **v18.0.11 与当前 main 在这段窗口逻辑上没有变化。** 当前 main 只把模型族到 counter 的映射改为 `quotaTierFor(...)`；daily/weekly 分类、`resetTime` 推断和 `DAY_MS`/`WEEK_MS` 输出仍相同。[当前 main 窗口逻辑](https://github.com/can1357/oh-my-pi/blob/65f79e76fcc89b96632fe86a598f314bd7cfc725/packages/ai/src/usage/google-antigravity.ts#L20-L114) [当前 main 输出逻辑](https://github.com/can1357/oh-my-pi/blob/65f79e76fcc89b96632fe86a598f314bd7cfc725/packages/ai/src/usage/google-antigravity.ts#L142-L154)

## OMP 如何产生 `1d`

固定版本的流程可以压缩成下面几步：

```text
fetchAvailableModels(project)
  -> models[model].quotaInfo / dailyQuotaInfo / weeklyQuotaInfo / quotaInfoByWindow
  -> 取 remainingFraction 与 resetTime
  -> 有 windowId/windowLabel：按名称分类
     无窗口名称：按 resetTime 与当前时间的距离推断
  -> UsageWindow { id, label, durationMs, resetsAt }
```

- `normalizeQuotaInfos()` 会把命名为 `dailyQuotaInfo` 的字段显式附上 `daily`/`Daily`，把 `weeklyQuotaInfo` 显式附上 `weekly`/`Weekly`；普通 `quotaInfo` 则不预设窗口。[固定版本归一化](https://github.com/can1357/oh-my-pi/blob/b8ce33a58911c26bed1d84f0db9a5e2e727c49a2/packages/ai/src/usage/google-antigravity.ts#L190-L217)
- `inferWindowDescriptors()` 对没有窗口名称的条目按 `(modelProvider, apiProvider, tier)` 分组。如果同组有多个不同重置时间，最晚的那个被当作 weekly；其余条目交给 `inferWindowFromReset()`。只有“距离现在超过一天”才会被判为 weekly。[固定版本推断](https://github.com/can1357/oh-my-pi/blob/b8ce33a58911c26bed1d84f0db9a5e2e727c49a2/packages/ai/src/usage/google-antigravity.ts#L69-L113)
- `withWindowDescriptor()` 会把推断出来的 `windowId`/`windowLabel` 回填到内存对象；`parseWindow()` 再把 descriptor 的 `durationMs` 与原始 `resetTime` 分别写成 `durationMs` 与 `resetsAt`。[固定版本回填与解析](https://github.com/can1357/oh-my-pi/blob/b8ce33a58911c26bed1d84f0db9a5e2e727c49a2/packages/ai/src/usage/google-antigravity.ts#L105-L153)
- 因此“`resetsAt` 还有 5 小时”与“`durationMs` 是 24 小时”可以同时出现：前者是服务器给出的下一次重置绝对时间，后者是 OMP 为 daily 类别使用的标准化窗口长度。

## 能否仅凭本机 normalized JSON 分辨来源？

**不能。** OMP 在内部已经用 `withWindowDescriptor()` 把 fallback 推断结果写回 `windowId`/`windowLabel`；输出阶段只保留标准化的 `window`。而 `omp usage --json` 会主动去掉每个报告的 provider-specific `raw` 字段，正是为了输出统一的 broker/API 形状。[OMP JSON 输出去除 raw](https://github.com/can1357/oh-my-pi/blob/65f79e76fcc89b96632fe86a598f314bd7cfc725/packages/coding-agent/src/cli/usage-cli.ts#L1143-L1147)

本仓库已有的脱敏 fixture 也显示了这个现象：Antigravity 报告的 `fetchedAt` 到 `resetsAt` 约 5 小时，但 normalized `durationMs` 仍是 `86400000`，而且 `window.id` 已经是 `daily`。[fixture](../../tests/fixtures/omp/usage-redacted.json#L206-L287)

这能证明 **duration 不是用 `resetsAt - fetchedAt` 现算的**，但不能单独证明服务器当时是否发送了 `windowId: "daily"`；要区分两者，必须在 OMP 归一化之前捕获原始 `fetchAvailableModels` 响应，或让 OMP 暴露窗口来源字段。

## Google 第一方资料如何解释“订阅却 daily”

Google Gemini CLI 的第一方文档把“付款方式/套餐”和“时间窗口”分开描述：

- Google account 的 Gemini Code Assist Individual 是每天 1,000 次；Google AI Pro 是每天 1,500 次；Google AI Ultra 是每天 2,000 次。[官方表格](https://github.com/google-gemini/gemini-cli/blob/0bd1d439751478771c45d3d0895a6a9760554bf4/docs/resources/quota-and-pricing.md#L13-L25)
- 文档把 paid tier 定义为 fixed-price subscription，并明确说它提供“more generous daily quotas”。[官方 paid-tier 说明](https://github.com/google-gemini/gemini-cli/blob/0bd1d439751478771c45d3d0895a6a9760554bf4/docs/resources/quota-and-pricing.md#L74-L97)
- Google Cloud 的配额文档还明确说 Gemini Code Assist agent mode 与 Gemini CLI 的请求配额合并，并且这些 daily request limits 会跨 Pro、Flash 等模型族聚合；达到每日上限后，相关接口要等 quota reset。[Google Cloud quotas](https://docs.cloud.google.com/gemini/docs/quotas#quotas_for_agent_mode_and_gemini_cli)

Google 官方开源 Gemini CLI 的 API 类型也把 quota bucket 建模为 `modelId`、`remainingFraction`、`resetTime`（可选的 `remainingAmount`/`tokenType`），而不是把订阅直接编码为 5h/7d。该客户端通过 `retrieveUserQuota` 读取这些 bucket。[官方 API 类型](https://github.com/google-gemini/gemini-cli/blob/0bd1d439751478771c45d3d0895a6a9760554bf4/packages/core/src/code_assist/types.ts#L236-L250) [官方客户端调用](https://github.com/google-gemini/gemini-cli/blob/0bd1d439751478771c45d3d0895a6a9760554bf4/packages/core/src/code_assist/server.ts#L344-L351)

## 对当前截图的准确解释

- 账号“登录成功”说明 OAuth 身份可用；`1d` 只说明 OMP 给该次 Antigravity quota counter 选了 daily 类别，**不能据此判断是免费账号，也不能据此否定 Google AI Pro/Ultra**。
- 如果该次响应是未带窗口名称的普通 `quotaInfo`，那么 1d 很可能是 OMP 的 `resetTime` fallback（例如重置在几小时内）；如果响应带了 `dailyQuotaInfo`/`windowId: daily`，则是服务端显式日窗口。normalized JSON 本身无法区分。
- 你当前侧边栏没有数字还有第二个独立原因：本插件的 OMP 适配目前只接受 5h、7d、monthly，故意丢弃 1d；这不是 OMP 报告“没有额度”，而是展示层尚未承载 daily 窗口。对应筛选逻辑见 [`src/providers/omp.rs`](../../src/providers/omp.rs#L168-L210)。

### 最短答案

**订阅是“你付费/所属的 tier”；`1d` 是“服务端这类请求的重置窗口”。Google 的付费订阅本来就可以是每日请求额度。你看到的 `86400000` 主要是 OMP 的标准化常量；仅凭 normalized 输出，不能证明上游明确返回了 daily，也不能证明账号不是订阅。**
