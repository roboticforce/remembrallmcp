#!/bin/sh
# RemembrallMCP installer - persistent memory for AI agents
# Usage: curl -fsSL https://retent.dev/install | sh
set -e

INSTALL_DIR="${REMEMBRALL_INSTALL_DIR:-$HOME/.remembrall/bin}"
REPO="roboticforce/remembrallmcp"

# Detect platform
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
  Darwin) os="apple-darwin" ;;
  Linux)  os="unknown-linux-gnu" ;;
  *)
    echo "Error: Unsupported operating system: $OS"
    echo "RemembrallMCP supports macOS and Linux."
    exit 1
    ;;
esac

case "$ARCH" in
  x86_64|amd64) arch="x86_64" ;;
  arm64|aarch64) arch="aarch64" ;;
  *)
    echo "Error: Unsupported architecture: $ARCH"
    echo "RemembrallMCP supports x86_64 and ARM64."
    exit 1
    ;;
esac

TARGET="${arch}-${os}"
TARBALL="remembrall-${TARGET}.tar.gz"

echo "Installing RemembrallMCP for ${TARGET}..."
echo ""

# Get latest release tag (or use REMEMBRALL_VERSION if set)
if [ -n "$REMEMBRALL_VERSION" ]; then
  VERSION="$REMEMBRALL_VERSION"
else
  VERSION=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"//;s/".*//')
  if [ -z "$VERSION" ]; then
    echo "Error: Could not determine latest version."
    echo "Set REMEMBRALL_VERSION=v0.1.0 to install a specific version."
    exit 1
  fi
fi

URL="https://github.com/${REPO}/releases/download/${VERSION}/${TARBALL}"

echo "  Version:  ${VERSION}"
echo "  Platform: ${TARGET}"
echo "  Install:  ${INSTALL_DIR}/remembrall"
echo ""

# Create install directory
mkdir -p "$INSTALL_DIR"

# Download and extract
echo "Downloading ${URL}..."
curl -fsSL "$URL" | tar xz -C "$INSTALL_DIR"

# Remove macOS quarantine flag if present
if [ "$OS" = "Darwin" ]; then
  xattr -d com.apple.quarantine "$INSTALL_DIR/remembrall" 2>/dev/null || true
fi

chmod +x "$INSTALL_DIR/remembrall"

# Add to PATH if not already there
case "$SHELL" in
  */zsh)
    RC_FILE="$HOME/.zshrc"
    ;;
  */bash)
    RC_FILE="$HOME/.bashrc"
    ;;
  */fish)
    RC_FILE="$HOME/.config/fish/config.fish"
    ;;
  *)
    RC_FILE=""
    ;;
esac

if [ -n "$RC_FILE" ]; then
  if ! echo "$PATH" | grep -q "$INSTALL_DIR"; then
    if [ "$(basename "$SHELL")" = "fish" ]; then
      echo "fish_add_path $INSTALL_DIR" >> "$RC_FILE"
    else
      echo "export PATH=\"$INSTALL_DIR:\$PATH\"" >> "$RC_FILE"
    fi
    echo "Added $INSTALL_DIR to PATH in $RC_FILE"
    echo "Run 'source $RC_FILE' or open a new terminal to use remembrall."
  fi
fi

echo ""
echo "RemembrallMCP installed successfully!"
echo ""
echo "Next steps:"
echo "  1. remembrall init              # Set up database"
echo "  2. Add to your project's .mcp.json:"
echo ""
echo '     { "mcpServers": { "remembrall": { "command": "remembrall" } } }'
echo ""
echo "  3. Restart your AI agent (Claude Code, Cursor, etc.)"
echo ""
