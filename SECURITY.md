# Security policy

## Reporting a vulnerability

Report privately through
[GitHub Security Advisories](https://github.com/levi-qiao/herdr-agent-quota/security/advisories/new).
Please do not open a public issue for a vulnerability. Expect a first response
within 7 days.

## What this plugin touches

Useful context when judging impact:

- **Reads** `~/.grok/auth.json` (login key only), Devin CLI's
  `credentials.toml` (API key only; never logged) and `config.json` (active
  model), Claude Code and Agy statusLine JSON on stdin, and the local
  `codex app-server` JSON-RPC socket.
- **Writes** sanitized percentages to Herdr's plugin state directory,
  `~/.config/herdr/config.toml`, `~/.claude/settings.json`, and
  `~/.gemini/antigravity-cli/settings.json`. Active-turn coordination locks, a
  configurable poll interval, and a temporary stop marker are also kept in
  that plugin state directory. Older plugin-owned Grok hook files may be
  removed during migration; user-owned hook content is never replaced.
- **Sends** authenticated quota requests using each CLI's own contract: Grok's
  billing endpoint, OpenCode Go's usage endpoint when a pane resolves to it,
  and Devin CLI's Connect RPC `GetUserStatus`. No usage data is uploaded
  anywhere else. API keys are never placed in logs, errors, or pane metadata.
- **Never** refreshes, rotates, or writes a provider credential, and never
  reads browser cookies or system keychains.

Credentials are held in memory for the duration of a single request and are
never written to the cache or logged.

## Supported versions

The latest release on `main`.
