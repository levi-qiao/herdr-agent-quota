# Contributing

Bug reports should include the plugin and CLI versions, reproduction steps,
and redacted expected/actual output. Never include credentials or private
session content. Report vulnerabilities through [SECURITY.md](SECURITY.md).

## Development

Use the toolchain pinned in `rust-toolchain.toml`. Tests use local fixtures and
stubs; installing the plugin into a running Herdr session is optional.

```sh
cargo fmt --all -- --check
cargo test --all-targets --all-features --locked
cargo clippy --release --all-targets --all-features --locked -- -D warnings
cargo build --release --locked
```

CI validates Linux and macOS, the plugin manifest, and dependency advisories.
Timing-sensitive tests can be diagnosed with `-- --test-threads=1`.

## Design requirements

- Use verified CLI contracts or local observations. Keep parsing separate from
  credential and network I/O; add redacted fixtures for each supported shape.
- Match quota to its credential or session evidence. A missing identity is not
  permission to reuse another account's cache. Preserve failed readings only
  when that attribution is still valid.
- Read only the named pane's visible screen on an event. Watch, refresh, and
  startup must not read terminal output. Publish once and suppress unchanged
  metadata; include new tokens in the metadata comparison set.
- Keep one bounded watcher for all supported harnesses. Fetch active or
  settling billing targets, and any target whose cached windows have expired,
  retain upstream cache limits, and test completion inside a debounce window.
- Preserve user configuration and existing preferences during upgrades.
  Installation, repair, and uninstall must be repeatable and reversible.
  Test migration from older caches and a watcher using an old Herdr client.
- Handle credentials in memory only. The plugin does not manage provider
  logins; an invoked CLI retains responsibility for its own credential lifecycle.

A harness and a billing provider are different concepts. Add a subscription
route only when the credential source is verified; use session diagnostics
without quota for unsupported or unconfirmed routes.

## Pull requests and documentation

Describe the user-visible problem, resulting behavior, and validation. Include
regression tests that exercise the actual caller path. Use conventional commit
prefixes such as `fix:`, `feat:`, and `docs:`.

Keep both READMEs aligned and concise. Record user-facing changes in
`CHANGELOG.md`; keep internal task plans out of public documentation. Historical
research belongs under `docs/research/` with dates and source links. See
[AGENTS.md](AGENTS.md) for repository-specific implementation constraints.
