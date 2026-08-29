# Contributing

Thanks for helping. Bug reports with a CLI version and a redacted payload are
the most useful contribution, because every parser here tracks a real provider
contract.

## Setup

Requires Rust `1.95+` (pinned by `rust-toolchain.toml`, so `rustup` installs it
for you) and Herdr `0.8.0+`.

```sh
git clone https://github.com/levi-qiao/herdr-agent-quota
cd herdr-agent-quota
./install.sh
```

## Before opening a pull request

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features --locked
```

CI runs exactly these on Linux and macOS.

## Design rules

These are the constraints the code is built around. A change that breaks one
needs to say so explicitly in the pull request.

1. **Local sources only.** Quota values come from a local file, a local CLI
   subprocess, or the contract the official CLI itself uses. No browser
   cookies, no keychain scraping, no private web endpoints.
2. **Read, never write, credentials.** The plugin never refreshes, rotates, or
   persists a provider token.
3. **A failed refresh keeps the last good value.** Losing a provider must never
   replace a working number with `unavailable`.
4. **Every user-facing config edit is reversible.** `configure --apply` backs up
   what it replaces and `configure --uninstall` restores it. Both are
   idempotent.
5. **No permanent resident processes.** StatusLine hooks are one-shot, the
   Codex app-server subprocess is killed and reaped before the command returns,
   and the active-turn refresh watcher is one bounded global worker. It polls
   once per configured interval, exits when all turns settle, and has a safety
   cap.

## Adding a billing collector

A Herdr harness is not automatically a billing collector. Add a `Harness`
classification without a `Provider` route when there is no verified
subscription contract and matching credential source. `Provider` remains the
legacy Rust name for billing identity while 0.2 cache and CLI compatibility is
preserved.

1. Add the variant to `Provider` in `src/model.rs`, including its aliases in
   the explicit harness-to-billing route only when the two identities truly
   match.
2. Add `src/providers/<name>.rs` with a pure `parse_*` function that takes
   `&serde_json::Value` and returns a `ProviderSnapshot`. Keep I/O in a
   separate `fetch` function so the parser stays unit-testable.
3. Add a redacted fixture under `tests/fixtures/<name>/` and a case in
   `tests/provider_contracts.rs`.
4. Reject payloads you cannot interpret. A wrong number is worse than `N/A` —
   see how `grok.rs` refuses a non-weekly billing period.

## Commit messages

Conventional commits (`feat:`, `fix:`, `docs:`, `chore:`), matching the
existing history.
