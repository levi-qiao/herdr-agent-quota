---
name: security-auditor
description: Security vulnerability assessment and secret detection — credential handling is load-bearing in this plugin
argument-hint: "[files or scope]"
agent: security-auditor
triggers:
  - user
  - model
---

Perform a comprehensive security audit of the herdr-agent-quota project.
This plugin reads credential stores (Claude, Codex, Grok, OpenCode, Pi,
omp), sends bearer tokens to provider billing endpoints, and reads omp's
`agent.db` (live OAuth tokens) and `models.db`. A leaked token or a
known-vulnerable dependency is a security incident, not a chore.

## Audit Focus

1. **Secret detection** — hardcoded keys/tokens, `.env` not in `.gitignore`,
   `tests/fixtures/**` must be synthetic/redacted (no real tokens, OAuth
   tokens, `agent.db`/`models.db` dumps), no secrets logged
   (`dbg!`/`println!`/`tracing`), no credentials in pane metadata tokens.
2. **Credential store access** — `agent.db` NEVER opened (live OAuth tokens);
   `models.db` read-only; provider credential files read with minimal
   permissions and never written (except `~/.claude/settings.json` via
   `configure`); bearer tokens over HTTPS, not logged on ureq errors.
3. **Input validation** — `HERDR_PLUGIN_EVENT_JSON` parsing tolerant and
   non-panicking; provider HTTP responses handled as untrusted input; omp
   transcript JSONL handles malformed lines; no path traversal.
4. **Dependency safety** — `cargo audit --deny warnings`; no floating
   `>=` without upper bound; TLS not disabled in `ureq`.
5. **Subprocess safety** — `omp usage --json --provider <id>` provider id
   is a known slug, not injectable; Codex app-server spawn args not injectable.
6. **Herdr socket protocol** — `HERDR_SOCKET_PATH` from env/config not
   hardcoded; one-request-one-reply, no persistent subscription.

See `.devin/agents/security-auditor/AGENT.md` for the full audit checklist.

## Scope
$ARGUMENTS

If no scope is provided, audit the current diff and credential-adjacent code
(`src/omp.rs`, `src/providers/**`, `src/opencode.rs`, `src/pi.rs`,
`src/herdr.rs`, `tests/fixtures/**`).

## Output Format
Provide:
- **Critical**: exploitable vulnerabilities or leaked credentials (fix immediately)
- **Warnings**: potential risks (fix soon)
- **Info**: best practice recommendations
- **PASS/FAIL** verdict (FAIL if any Critical issues found)

## Tools
- `cargo audit --deny warnings` for dependency vulnerabilities
- `grep`/`rg` for secret patterns in code and fixtures
