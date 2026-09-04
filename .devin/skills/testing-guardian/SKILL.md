---
name: testing-guardian
description: Test coverage, quality, and isolation review — no live services in CI, no leaked credentials in fixtures
argument-hint: "[files or scope]"
agent: testing-guardian
triggers:
  - user
  - model
---

Review test coverage and quality for the herdr-agent-quota project.

## Review Focus

1. **Test isolation** — no live Herdr, no live provider endpoints, no network
   in CI. `#[ignore]`'d tests only run with `HERDR_SOCK`/`HERDR_SOCKET_PATH`
   present. Provider HTTP stubbed with `tests/fixtures/**`. omp `usage --json`
   shell-out stubbed, never spawned. `tempfile::tempdir()` for DBs/sockets
   (AF_UNIX 108-char limit).
2. **Coverage gaps** — provider collectors cover happy path, missing fields,
   malformed JSON, empty response, quota window edges (5h, 7d, weekly,
   monthly). omp `credential_pin` pinned-digest test. `metadata_matches`
   token-coverage test. `configure` round-trip (apply → uninstall → re-apply)
   for every agent. Error paths tested.
3. **Test quality** — specific assertions (not just `is_ok()`), one thing per
   test, descriptive names.
4. **No wall-clock flakiness** — injected times, not `now_utc()`/`SystemTime::now()`.
5. **Credential safety in fixtures** — `tests/fixtures/**` synthetic/redacted;
   flag anything resembling a real credential.
6. **Pane-discipline regression tests** — if `src/herdr.rs` or `src/refresh.rs`
   changed, verify tests guard against fan-out reads, double publish, and
   `metadata_matches` gaps.

See `.devin/agents/testing-guardian/AGENT.md` for the full review checklist.

## Scope
$ARGUMENTS

If no scope is provided, review `tests/**` and any `#[cfg(test)]` modules in
the current diff.

## Output Format
Provide:
- **Coverage assessment**: what is tested, what is missing
- **Issues**: file path, line number, severity
- **Test quality**: are existing tests meaningful?
- **Credential safety**: any fixture that looks like a real secret
- **PASS/FAIL** verdict

## Tools
- `cargo test --all-targets --all-features --locked` for test execution
