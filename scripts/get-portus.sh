#!/usr/bin/env bash
set -euo pipefail

REPO="PerceivingAI/portus-window"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"
SKILLS_DIR="${HOME}/.claude/skills/portus-window"

echo "==> Installing Portus Window for Linux..."

ARCH="$(uname -m)"
case "$ARCH" in
    x86_64) ARCH_SUFFIX="x86_64" ;;
    aarch64|arm64) ARCH_SUFFIX="aarch64" ;;
    *) echo "Unsupported architecture: $ARCH" >&2; exit 1 ;;
esac

TAG=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" 2>/dev/null | grep '"tag_name":' | sed -E 's/.*"([^"]+)".*/\1/' || echo "latest")
TARBALL="portus-window-linux-${ARCH_SUFFIX}.tar.gz"
URL="https://github.com/${REPO}/releases/download/${TAG}/${TARBALL}"

TEMP_DIR=$(mktemp -d)
trap 'rm -rf "$TEMP_DIR"' EXIT

echo "==> Downloading ${TARBALL} (${TAG})..."
if ! curl -fsSL "$URL" -o "$TEMP_DIR/$TARBALL" 2>/dev/null; then
    echo "Precompiled release binary not found at $URL. Building from repository via cargo..."
    cargo install --git "https://github.com/${REPO}.git" portus-window portus-window-cli
    exit 0
fi

tar -xzf "$TEMP_DIR/$TARBALL" -C "$TEMP_DIR"

mkdir -p "$INSTALL_DIR"
cp "$TEMP_DIR/portus-window" "$INSTALL_DIR/"
cp "$TEMP_DIR/portus-window-cli" "$INSTALL_DIR/"
chmod +x "$INSTALL_DIR/portus-window" "$INSTALL_DIR/portus-window-cli"

echo "✓ Binaries installed to $INSTALL_DIR"

if [[ -d "$TEMP_DIR/skills/portus-window" ]]; then
    mkdir -p "$SKILLS_DIR" "${HOME}/.agents/skills/portus-window" "${HOME}/.codex/skills/portus-window"
    cp -r "$TEMP_DIR/skills/portus-window/"* "$SKILLS_DIR/"
    cp -r "$TEMP_DIR/skills/portus-window/"* "${HOME}/.agents/skills/portus-window/"
    cp -r "$TEMP_DIR/skills/portus-window/"* "${HOME}/.codex/skills/portus-window/"
    echo "✓ Agent skill installed to $SKILLS_DIR"
fi

echo ""
echo "Portus Window installation complete!"
echo "Ensure $INSTALL_DIR is in your PATH."
