#!/usr/bin/env sh
# claude-trace-rs installer for macOS and Linux.
#
#   curl -fsSL https://raw.githubusercontent.com/CodeHalwell/claude-trace-rs/main/scripts/install.sh | sh
#
# Downloads the latest release archive for your platform and installs the
# binary into ~/.local/bin (or $CLAUDE_TRACE_INSTALL_DIR).
set -eu

REPO="CodeHalwell/claude-trace-rs"
BIN="claude-trace-rs"
INSTALL_DIR="${CLAUDE_TRACE_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf '\033[1;34m==>\033[0m %s\n' "$1"; }
err() { printf '\033[1;31merror:\033[0m %s\n' "$1" >&2; exit 1; }

os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
  Linux)  os_t="unknown-linux-gnu" ;;
  Darwin) os_t="apple-darwin" ;;
  *) err "unsupported OS: $os (use the Windows installer or build from source)" ;;
esac
case "$arch" in
  x86_64|amd64) arch_t="x86_64" ;;
  arm64|aarch64) arch_t="aarch64" ;;
  *) err "unsupported architecture: $arch" ;;
esac
target="${arch_t}-${os_t}"

say "Resolving latest release…"
tag="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
  | grep -m1 '"tag_name"' | cut -d'"' -f4)"
[ -n "$tag" ] || err "could not determine latest release tag"
version="${tag#v}"

asset="${BIN}-${version}-${target}.tar.gz"
url="https://github.com/${REPO}/releases/download/${tag}/${asset}"
say "Downloading ${asset}…"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
curl -fsSL "$url" -o "$tmp/$asset" || err "download failed: $url"
tar -C "$tmp" -xzf "$tmp/$asset"

mkdir -p "$INSTALL_DIR"
cp "$tmp/${BIN}-${version}-${target}/${BIN}" "$INSTALL_DIR/$BIN"
chmod +x "$INSTALL_DIR/$BIN"

say "Installed ${BIN} ${version} to ${INSTALL_DIR}/${BIN}"
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) printf '\033[1;33mnote:\033[0m add %s to your PATH:\n  export PATH="%s:$PATH"\n' "$INSTALL_DIR" "$INSTALL_DIR" ;;
esac
say "Run it with:  ${BIN} serve --open"
