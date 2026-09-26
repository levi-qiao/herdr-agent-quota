# OpenCode Go: key counters versus console meters

> Research date: 2026-09-24 (UTC), observed on a live Go subscription.
> Supersedes the single-source assumptions in
> [opencode-go-usage.md](opencode-go-usage.md) where they differ. That note
> documented the endpoint shape; this one records which meter belongs to which
> principal.

## Two meters, two principals

| Source | Authenticated as | Measures |
| --- | --- | --- |
| `GET https://opencode.ai/zen/go/v1/usage` | the Go API key (`auth.json` `opencode-go`) | what **that key** spent: `usage.{rolling,weekly,monthly}.percent`, `percent` is used 0..100 |
| `GET https://opencode.ai/console/api/go/status` | the console login OpenCode keeps in its own database | what **the account** spent: `access.meters.{fiveHour,week,month}` as `usedMicroCents`/`limitMicroCents`, where 100,000,000 microcents is one dollar |

The console page renders exactly the second source:
`round(usedMicroCents / limitMicroCents × 100)`; the monthly countdown uses
`access.endsAt` (the paid period), not the key endpoint's monthly `resetsAt`.

## Observed divergence (2026-09-24T22:17Z)

| Window | Key endpoint | Console meters |
| --- | --- | --- |
| 5-hour | 0%, `resetsAt` = query time + 5h | $1.69 / $12 = 14.1% |
| Week | 99%, resets 2026-09-28T00:00Z | $17.95 / $30 = 59.8% |
| Month | 71%, resets 2026-10-16T12:14Z | $53.25 / $60 = 88.8%, period 2026-09-11 → 2026-10-11 |

Why they diverge on one account:

- OpenCode 2 serves Go inference as the **signed-in console account**. The
  account's usage rows for that day read `principalType: user`,
  `app: opencode`, `billingSource: go`; none carried a service key id.
- The `opencode-go` key had not served traffic since the console login appeared
  (2026-09-24T00:00Z, the credential's `time_created`). Its rolling window only
  restarted when the key itself was probed, and its weekly/monthly percentages
  stayed frozen while the account kept spending.
- The two monthly windows have different anchors: the key endpoint's 30-day
  window reset 2026-10-16T12:14:01Z; the console month resets with the paid
  period (2026-10-11T10:09:35Z here).

A key that an install stopped using therefore freezes at its last reading. It
is not a live view of the panes beside it.

## Credential source

OpenCode 2 stores the console login in its own database: table `credential`,
`integration_id = 'opencode'`, value JSON with `access`, `refresh`, `expires`
and `metadata.{server, accountID, email, orgID, orgName}`. The plugin reads
only `access` and `metadata` (never `refresh`), sends the token only to the
login's own `https://opencode.ai` host with `x-org-id: <orgID>`, and never logs
it. OpenCode 1 stores have no `credential` table, so there is nothing to read
and the key endpoint stays in use.

## Implementation

- `src/opencode.rs`: `console_credential`, `go_account_id`
- `src/providers/opencode_go.rs`: `fetch_console`, `parse_console_status`
- `src/refresh.rs`: the console is preferred for a resolved OpenCode pane; the
  key endpoint is the fallback when the store has no console login. A pane
  that has not started a session yet takes the same console target, so limits
  appear before the first turn; the first resolved session replaces or clears
  them.

## Open questions

- Whether the console meters include usage billed to service-account keys.
  The org usage export carries `service_account_name` when a request used one;
  none was observed here, because the account's key had no recent traffic.
- An expired console login is not refreshed by the plugin. The CLI owns the
  token; until it refreshes, the collector keeps the last verified reading.
