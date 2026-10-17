#!/bin/sh
# Installs the latest GANG release on macOS or Linux.
#
#   curl -fsSL https://raw.githubusercontent.com/AyberkA0/gang/main/install.sh | sh
#
# While the repository is private, have the GitHub CLI (`gh`) signed in.
# GANG_INSTALL_DIR chooses where the binary goes (default: ~/.local/bin).
set -eu

REPO="AyberkA0/gang"
DIR="${GANG_INSTALL_DIR:-$HOME/.local/bin}"

case "$(uname -s)" in
  Darwin) os="apple-darwin" ;;
  Linux) os="unknown-linux-gnu" ;;
  *) echo "Unsupported system: $(uname -s). On Windows use install.ps1." >&2; exit 1 ;;
esac
case "$(uname -m)" in
  x86_64 | amd64) arch="x86_64" ;;
  arm64 | aarch64) arch="aarch64" ;;
  *) echo "Unsupported processor: $(uname -m)" >&2; exit 1 ;;
esac
asset="gang-$arch-$os.tar.gz"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "Downloading $asset..."
if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
  gh release download --repo "$REPO" --pattern "$asset" --dir "$tmp"
else
  curl -fsSL -o "$tmp/$asset" "https://github.com/$REPO/releases/latest/download/$asset" || {
    echo "Download failed. If the repository is private, install and sign in to the GitHub CLI (gh auth login) first." >&2
    exit 1
  }
fi

tar xzf "$tmp/$asset" -C "$tmp"
mkdir -p "$DIR"
mv "$tmp/gang" "$DIR/gang"
chmod +x "$DIR/gang"
echo "Installed $("$DIR/gang" --version) to $DIR/gang"
case ":$PATH:" in
  *":$DIR:"*) ;;
  *) echo "Add $DIR to your PATH to run gang from anywhere." ;;
esac
