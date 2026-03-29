#!/bin/sh
# rookery installer - https://github.com/lance0/rookery
# Usage: curl -fsSL https://raw.githubusercontent.com/lance0/rookery/main/install.sh | sh

set -e

REPO="lance0/rookery"
INSTALL_DIR="${INSTALL_DIR:-/usr/local/bin}"

# Detect OS and architecture
OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)

case "$OS" in
  linux)
    case "$ARCH" in
      x86_64)  TARGET="x86_64-unknown-linux-gnu" ;;
      aarch64) TARGET="aarch64-unknown-linux-gnu" ;;
      arm64)   TARGET="aarch64-unknown-linux-gnu" ;;
      *) echo "Unsupported architecture: $ARCH"; exit 1 ;;
    esac
    ;;
  *)
    echo "Rookery requires Linux with an NVIDIA GPU."
    echo "Unsupported OS: $OS"
    exit 1
    ;;
esac

# Get latest version
VERSION=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" | grep '"tag_name"' | cut -d'"' -f4)
if [ -z "$VERSION" ]; then
  echo "Failed to get latest version"
  exit 1
fi

URL="https://github.com/$REPO/releases/download/$VERSION/rookery-$TARGET.tar.gz"

echo "Installing rookery $VERSION for $TARGET..."

# Download and extract
TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

curl -fsSL "$URL" | tar xz -C "$TMPDIR"

# Create install directory if it doesn't exist
if [ ! -d "$INSTALL_DIR" ]; then
  echo "Creating $INSTALL_DIR..."
  sudo mkdir -p "$INSTALL_DIR"
fi

# Install binaries
if [ -w "$INSTALL_DIR" ]; then
  mv "$TMPDIR/rookeryd" "$INSTALL_DIR/rookeryd"
  mv "$TMPDIR/rookery" "$INSTALL_DIR/rookery"
else
  echo "Installing to $INSTALL_DIR (requires sudo)..."
  sudo mv "$TMPDIR/rookeryd" "$INSTALL_DIR/rookeryd"
  sudo mv "$TMPDIR/rookery" "$INSTALL_DIR/rookery"
fi

echo ""
echo "Installed rookeryd and rookery to $INSTALL_DIR"
echo ""
echo "Next steps:"
echo "  1. Create config:  mkdir -p ~/.config/rookery && cp config.example.toml ~/.config/rookery/config.toml"
echo "  2. Edit config:    \$EDITOR ~/.config/rookery/config.toml"
echo "  3. Start daemon:   rookeryd &"
echo "  4. Or with systemd: sudo make install  (from source checkout for systemd unit)"
echo ""
echo "Run 'rookery --help' to get started"
