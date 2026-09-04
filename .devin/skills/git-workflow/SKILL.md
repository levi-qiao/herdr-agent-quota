---
name: git-workflow
description: Git operations, branch management, and commit validation
argument-hint: "[operation or scope]"
agent: git-workflow
triggers:
  - user
  - model
---

Handle git operations and workflow tasks for the herdr-agent-quota project:

1. **Commit management** — message quality and conventional-commit format,
   staging appropriate files, history cleanliness.
2. **Branch management** — `feature/`, `bugfix/`, `config/` naming; never
   commit on `main`; stash before branching if needed.
3. **Code review preparation** — PR description, diff clarity, reviewer
   assignment.
4. **Workflow validation** — pre-commit gates (`cargo fmt --all -- --check`,
   `cargo clippy --all-targets --all-features -- -D warnings`) pass before
   committing Rust changes.
5. **History maintenance** — squash/fixup when appropriate, rebase safety,
   no force-push without explicit approval.

## Project-specific
- `herdr-plugin.toml` version must match `Cargo.toml` version.
- Never commit real tokens, keys, or `agent.db`/`models.db` dumps — test
  fixtures under `tests/fixtures/**` must stay synthetic/redacted.
- Build contract: `cargo build --release` → `./target/release/herdr-agent-quota`.
- Reload after rebuild: `herdr plugin disable herdr-agent-quota && herdr plugin enable herdr-agent-quota`.

See `.devin/agents/git-workflow/AGENT.md` for the full rules.

## Operation
$ARGUMENTS

If no operation is specified, assess the current git state and provide recommendations.

## Output Format
Provide:
- Current git status summary
- Specific action recommendations
- Command examples where applicable
- Risk assessments for destructive operations
