# OpenCode Go 用量端点研究

> 研究日期：2026-08-29（Asia/Shanghai）
> 复核版本：CodexBar `b366a2d5aa52047524a8b9177a99e2a1c1eedd70`（2026-08-28）；
> opencode `1.18.20`（Homebrew core，本机实测）；models.dev API 快照同日。
> 目标：在**本仓库维护者没有 OpenCode Go 订阅**的前提下，尽可能靠一手证据确定
> `/zen/go/v1/usage` 的请求方式与响应形状，避免凭空编造 schema。

## 结论先行

端点真实存在，响应形状可从 CodexBar 的实现与其测试用例中确定。两处与本仓库
`.agents/plans/harness-billing-coverage.md` §8 原先的假设**不一致**，已按证据更正：

| 项 | 原计划假设 | 实际证据 |
|---|---|---|
| 重置时间字段 | `resetsAt`（时间戳） | 主字段是 `resetInSec`（整数秒），`resetsAt` 仅作兼容 |
| `status` 字段 | 每个窗口有 `status` | 观测到的响应里没有；CodexBar 也不读它 |
| `percent` 语义 | 「used percent 0..=100」 | 确认正确：API 路径就是 0..100 |

## 端点存在性（本机实测，无需订阅）

```
$ curl -s -o /dev/null -w '%{http_code}' https://opencode.ai/zen/go/v1/usage
401
$ curl -s -H 'Authorization: Bearer invalid-probe' https://opencode.ai/zen/go/v1/usage
{"type":"error","error":{"type":"AuthError","message":"Unauthorized"}}
$ curl -s -o /dev/null -w '%{http_code}' https://opencode.ai/zen/go/v1/models
200
```

401 而非 404，说明路径本身有效；`/v1/models` 作为对照返回 200。错误体形状
`{"type":"error","error":{...}}` 由此确定。

opencode CLI 自身**从不**调用这个端点（扫描 `opencode.exe` 内全部
`https://opencode.ai/*` 字面量，只有 console / docs / auth 等），官方 go 文档也只
记录了 `/v1/models`，把用量查询指向 web console。因此上游没有可直接引用的
schema，必须依赖第三方实现。

## 请求形状（CodexBar）

```text
GET https://opencode.ai/zen/go/v1/usage
Authorization: Bearer <key>
Accept: application/json
```

见 [`OpenCodeGoUsageFetcher.swift`](https://github.com/steipete/CodexBar/blob/b366a2d5aa52047524a8b9177a99e2a1c1eedd70/Sources/CodexBarCore/Providers/OpenCodeGo/OpenCodeGoUsageFetcher.swift#L31)
（`usageAPIURL`）与同文件的请求构造（`Bearer` + `Accept: application/json`）。
`401`/`403` 被映射为 `invalidCredentials`，**不会**退化成 0% 窗口。

## 响应形状（CodexBar 测试用例）

CodexBar 自己的测试直接把下面这段喂给 `parseAPIUsage`：

```json
{"usage": {
  "rolling": {"percent": 3, "resetInSec": 18100},
  "weekly":  {"percent": 1, "resetInSec": 266500},
  "monthly": {"percent": 0, "resetInSec": 1539100}
}}
```

并断言 `primary.usedPercent == 3`、`secondary == 1`、`tertiary == 0`
（[`OpenCodeGoWebOverlayTests.swift`](https://github.com/steipete/CodexBar/blob/b366a2d5aa52047524a8b9177a99e2a1c1eedd70/Tests/CodexBarTests/OpenCodeGoWebOverlayTests.swift#L263-L282)）。

### percent 的语义（关键）

`parseAPIUsage` 以 `directPercentEncoding: .percent` 调用 `buildSnapshot`，源码注释写明：

> Dashboard JSON may use fractions. **API fields** and computed used/limit
> percentages **already use 0...100**.

`fractionOrPercent` 分支才会把 `<= 1.0` 的值乘 100；`.percent` 分支不做这个转换。
所以 API 返回的 `0.5` 就是 **0.5%**，不是 50%。这条是本插件最容易出 100 倍错误的
地方，实现里必须有专门测试钉住。

### 字段容差

CodexBar 对每个窗口按顺序尝试多个键名，本插件按需取其中被证据支持的子集：

- 百分比：`percent`（测试用例实测）；CodexBar 另接受 `usagePercent` / `usedPercent`
  / `percentUsed` 等别名。
- 重置：先 `resetInSec` 系列（`resetInSec` / `resetInSeconds` / …），再 `resetAt`
  / `resetsAt` 日期系列。CodexBar 的错误测试里出现过 `resetsAt`，说明两种都可能。
- `rolling` 为必需；`weekly` / `monthly` 缺失即视为该窗口不存在，而不是 0%。

## 凭据来源（与本仓库既有实现一致）

CodexBar 取 Go key 的两条路径与本插件 Goal 2 已实现的完全一致：

- `~/.local/share/opencode/auth.json` 中 `opencode-go` 条目的 `key`
  （[`OpenCodeGoLocalUsageReader.swift`](https://github.com/steipete/CodexBar/blob/b366a2d5aa52047524a8b9177a99e2a1c1eedd70/Sources/CodexBarCore/Providers/OpenCodeGo/OpenCodeGoLocalUsageReader.swift#L230-L239)）
- 环境变量 `OPENCODE_API_KEY`
  （[`OpenCodeGoSettingsReader.swift`](https://github.com/steipete/CodexBar/blob/b366a2d5aa52047524a8b9177a99e2a1c1eedd70/Sources/CodexBarCore/Providers/OpenCodeGo/OpenCodeGoSettingsReader.swift#L4)）

CodexBar 判定本地会话归属时用的 SQL 也是
`json_extract(data, '$.providerID') = 'opencode-go'`，与本插件的会话解析口径相同。

## 本仓库不采用的部分

CodexBar 还实现了 web console cookie 抓取、`_server` RPC、Zen 余额、本地成本估算与
日histogram。本插件明确不做：计划书禁止浏览器 cookie、Keychain 与本地估算，且这些
路径都依赖 macOS 特有能力或会触发钥匙串授权。**只保留 Bearer + 官方 REST 一条路。**

## 证据等级与未决项

- **一手实测**：端点存在性、401 错误体、opencode 本地存储与 provider id。
- **二手可信**：成功响应的字段名与 percent 语义 —— 来自 CodexBar 的实现和它自己的
  测试。CodexBar 有真实订阅用户在用，但这仍不是本仓库维护者亲眼看到的线上响应。
- **未验证**：真实线上响应是否还带有其他字段；`status` 是否在某些账户状态下出现；
  限流（429）时的响应体；`resetInSec` 与 `resetsAt` 在当前部署中究竟用哪个。

本仓库维护者没有 OpenCode Go 订阅，无法补上最后一项。实现按「缺字段就 fail closed、
永不猜数值」处理，欢迎有订阅的协作者用真实响应校正并提 PR。
