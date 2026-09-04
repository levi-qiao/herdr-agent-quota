#!/usr/bin/env python3
"""Update model: fields in AGENT.md files from .devin/models.json.

Usage:
    python3 .devin/scripts/update_models.py              # Apply models.json to all AGENT.md files
    python3 .devin/scripts/update_models.py --dry-run     # Show what would change without writing
    python3 .devin/scripts/update_models.py --list        # List current models from models.json
    python3 .devin/scripts/update_models.py --diff        # Show differences between models.json and AGENT.md files

The models.json file is the single source of truth for model assignments.
Skill files (SKILL.md) should NOT have a model: field — they inherit from
their AGENT.md profile.
"""

import argparse
import json
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
AGENTS_DIR = REPO_ROOT / ".devin" / "agents"
SKILLS_DIR = REPO_ROOT / ".devin" / "skills"
MODELS_FILE = REPO_ROOT / ".devin" / "models.json"

MODEL_RE = re.compile(r"^model:\s*(\S+)\s*$", re.MULTILINE)


def load_models_config() -> dict:
    """Load and validate the models.json config file."""
    if not MODELS_FILE.exists():
        print(f"Error: {MODELS_FILE} not found", file=sys.stderr)
        sys.exit(1)
    with MODELS_FILE.open("r", encoding="utf-8") as f:
        config = json.load(f)
    # If agents key is missing, all agents use the default
    if "agents" not in config:
        config["agents"] = {}
    return config


def get_agent_dirs() -> list[Path]:
    """Get all agent profile directories."""
    if not AGENTS_DIR.exists():
        return []
    return sorted(
        d for d in AGENTS_DIR.iterdir() if d.is_dir() and (d / "AGENT.md").exists()
    )


def get_skill_dirs() -> list[Path]:
    """Get all skill directories."""
    if not SKILLS_DIR.exists():
        return []
    return sorted(
        d for d in SKILLS_DIR.iterdir() if d.is_dir() and (d / "SKILL.md").exists()
    )


def read_current_model(agent_md: Path) -> str | None:
    """Read the current model: value from an AGENT.md file."""
    text = agent_md.read_text(encoding="utf-8")
    match = MODEL_RE.search(text)
    return match.group(1) if match else None


def update_model_in_file(
    file_path: Path, new_model: str, dry_run: bool = False
) -> bool:
    """Update or insert the model: field in a frontmatter file.

    Returns True if a change was made (or would be made in dry-run mode).
    """
    text = file_path.read_text(encoding="utf-8")
    match = MODEL_RE.search(text)

    if match:
        old_model = match.group(1)
        if old_model == new_model:
            return False  # already correct
        new_text = MODEL_RE.sub(f"model: {new_model}", text)
    else:
        # Insert model: after the last frontmatter field before the closing ---
        # Find the closing --- of the frontmatter
        lines = text.split("\n")
        if not lines or not lines[0].strip().startswith("---"):
            print(
                f"  Warning: {file_path.name} has no frontmatter, skipping",
                file=sys.stderr,
            )
            return False
        insert_idx = None
        for i in range(1, len(lines)):
            if lines[i].strip() == "---":
                insert_idx = i
                break
        if insert_idx is None:
            print(
                f"  Warning: {file_path.name} has no closing ---, skipping",
                file=sys.stderr,
            )
            return False
        lines.insert(insert_idx, f"model: {new_model}")
        new_text = "\n".join(lines)

    if not dry_run:
        file_path.write_text(new_text, encoding="utf-8")
    return True


def remove_model_from_skill(file_path: Path, dry_run: bool = False) -> bool:
    """Remove the model: field from a SKILL.md file.

    Returns True if a line was removed (or would be removed in dry-run mode).
    """
    text = file_path.read_text(encoding="utf-8")
    match = MODEL_RE.search(text)
    if not match:
        return False  # no model: field to remove

    # Remove the model: line and any trailing blank line that would leave
    # a double-blank inside the frontmatter
    lines = text.split("\n")
    new_lines = []
    skip_next_blank = False
    for i, line in enumerate(lines):
        if MODEL_RE.match(line):
            skip_next_blank = True
            continue
        if skip_next_blank and line.strip() == "":
            skip_next_blank = False
            continue
        skip_next_blank = False
        new_lines.append(line)

    new_text = "\n".join(new_lines)
    # Clean up any remaining double-blank lines
    new_text = re.sub(r"\n{3,}", "\n\n", new_text)

    if not dry_run:
        file_path.write_text(new_text, encoding="utf-8")
    return True


def cmd_list(config: dict) -> int:
    """List all agent→model mappings from models.json."""
    default = config.get("_default_model", "unspecified")
    agents_config = config["agents"]
    print(f"Default model: {default}")
    print(f"\n{'Agent':<30} {'Model'}")
    print(f"{'─' * 30} {'─' * 20}")
    for agent_dir in get_agent_dirs():
        name = agent_dir.name
        configured = agents_config.get(name)
        # Empty string means use default
        model = default if configured == "" or configured is None else configured
        print(f"{name:<30} {model}")
    return 0


def cmd_diff(config: dict) -> int:
    """Show differences between models.json and AGENT.md files."""
    default = config.get("_default_model", "glm-5-2-high")
    agents_config = config["agents"]
    changes = 0

    print(f"{'Agent':<30} {'AGENTS.md':<20} {'models.json':<20} {'Status'}")
    print(f"{'─' * 30} {'─' * 20} {'─' * 20} {'─' * 10}")

    for agent_dir in get_agent_dirs():
        name = agent_dir.name
        agent_md = agent_dir / "AGENT.md"
        current = read_current_model(agent_md)
        configured = agents_config.get(name)
        # Empty string means use default
        desired = default if configured == "" or configured is None else configured

        if current == desired:
            status = "✓"
        else:
            status = "CHANGED"
            changes += 1

        # Show the configured value (empty string for default)
        config_display = configured if configured is not None else "(not in config)"
        if configured == "":
            config_display = "(empty → default)"

        print(f"{name:<30} {str(current):<20} {config_display:<20} {status}")

    # Check for agents in config but missing directory
    for name in agents_config:
        if not (AGENTS_DIR / name / "AGENT.md").exists():
            print(f"{name:<30} {'(missing)':<20} {agents_config[name]:<20} MISSING")
            changes += 1

    if changes == 0:
        print("\n✓ All models match models.json")
    else:
        print(f"\n{changes} difference(s) found. Run without --dry-run to apply.")
    return 0


def cmd_apply(config: dict, dry_run: bool = False) -> int:
    """Apply models.json to all AGENT.md files and clean skills."""
    default = config.get("_default_model", "glm-5-2-high")
    agents_config = config["agents"]
    updated = 0
    skipped = 0
    errors = 0

    # --- Update AGENT.md files ---
    print("=== Updating AGENT.md files ===")
    for agent_dir in get_agent_dirs():
        name = agent_dir.name
        agent_md = agent_dir / "AGENT.md"
        configured_model = agents_config.get(name)

        # If agent is not in config or has empty string, use default
        if configured_model is None or configured_model == "":
            desired_model = default
            if configured_model == "":
                print(f"  ℹ {name}: empty override, using default ({default})")
            else:
                print(f"  ℹ {name}: not in models.json, using default ({default})")
        else:
            desired_model = configured_model

        current = read_current_model(agent_md)
        if current == desired_model:
            skipped += 1
            continue

        if dry_run:
            print(f"  ~ {name}: {current or '(none)'} → {desired_model} (dry-run)")
        else:
            if update_model_in_file(agent_md, desired_model):
                print(f"  ✓ {name}: {current or '(none)'} → {desired_model}")
            else:
                print(f"  ⚠ {name}: update failed")
                errors += 1
                continue
        updated += 1

    # --- Remove model: from SKILL.md files ---
    print("\n=== Cleaning SKILL.md files (removing model:) ===")
    skills_cleaned = 0
    for skill_dir in get_skill_dirs():
        skill_md = skill_dir / "SKILL.md"
        if dry_run:
            if MODEL_RE.search(skill_md.read_text(encoding="utf-8")):
                print(f"  ~ {skill_dir.name}: would remove model: (dry-run)")
                skills_cleaned += 1
        else:
            if remove_model_from_skill(skill_md):
                print(f"  ✓ {skill_dir.name}: removed model:")
                skills_cleaned += 1

    # --- Summary ---
    print("\n=== Summary ===")
    print(f"  AGENT.md files updated: {updated}")
    print(f"  AGENT.md files skipped (already correct): {skipped}")
    print(f"  SKILL.md files cleaned: {skills_cleaned}")
    if errors:
        print(f"  Errors: {errors}", file=sys.stderr)
    if dry_run:
        print("  (dry-run — no files were modified)")
    else:
        print("  All changes applied.")
    return 1 if errors else 0


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Update model: fields in AGENT.md files from .devin/models.json."
    )
    parser.add_argument(
        "--dry-run", action="store_true", help="Show changes without writing files"
    )
    parser.add_argument(
        "--list", action="store_true", help="List agent→model mappings from models.json"
    )
    parser.add_argument(
        "--diff",
        action="store_true",
        help="Show differences between models.json and AGENT.md files",
    )
    args = parser.parse_args()

    config = load_models_config()

    if args.list:
        return cmd_list(config)
    if args.diff:
        return cmd_diff(config)
    return cmd_apply(config, dry_run=args.dry_run)


if __name__ == "__main__":
    sys.exit(main())
