#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
SKILL_SOURCE="$REPO_ROOT/skills/portus-window"

if [[ ! -d "$SKILL_SOURCE" ]]; then
    echo "Error: Skill directory not found at $SKILL_SOURCE" >&2
    exit 1
fi

MODE="${1:---global}"

case "$MODE" in
    --global)
        TARGET_DIR="${HOME}/.claude/skills/portus-window"
        AGENTS_DIR="${HOME}/.agents/skills/portus-window"
        CODEX_DIR="${HOME}/.codex/skills/portus-window"
        mkdir -p "${HOME}/.claude/skills" "${HOME}/.agents/skills" "${HOME}/.codex/skills"
        rm -rf "$TARGET_DIR" "$AGENTS_DIR" "$CODEX_DIR"
        cp -r "$SKILL_SOURCE" "$TARGET_DIR"
        cp -r "$SKILL_SOURCE" "$AGENTS_DIR"
        cp -r "$SKILL_SOURCE" "$CODEX_DIR"
        echo "✓ Installed Portus Window skill globally to:"
        echo "  - $TARGET_DIR"
        echo "  - $AGENTS_DIR"
        echo "  - $CODEX_DIR"
        ;;
    --local)
        TARGET_DIR="${REPO_ROOT}/.claude/skills/portus-window"
        mkdir -p "${REPO_ROOT}/.claude/skills"
        rm -rf "$TARGET_DIR"
        cp -r "$SKILL_SOURCE" "$TARGET_DIR"
        echo "✓ Installed Portus Window skill locally to:"
        echo "  - $TARGET_DIR"
        ;;
    *)
        echo "Usage: $0 [--global | --local]" >&2
        exit 1
        ;;
esac
