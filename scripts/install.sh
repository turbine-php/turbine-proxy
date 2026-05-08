#!/bin/sh

set -eu

REPO="${TURBINEPROXY_REPO:-turbineproxy/turbineproxy}"
INSTALL_DIR="${TURBINEPROXY_INSTALL_DIR:-/usr/local/bin}"
VERSION="${1:-${TURBINEPROXY_VERSION:-latest}}"
BINARY_NAME="turbineproxy"

need_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Error: required command '$1' not found" >&2
    exit 1
  fi
}

need_cmd uname
need_cmd curl
need_cmd mktemp
need_cmd install

OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"

case "$ARCH" in
  x86_64|amd64)
    ARCH="x86_64"
    ;;
  arm64|aarch64)
    ARCH="aarch64"
    ;;
  *)
    echo "Error: unsupported architecture '$ARCH'" >&2
    exit 1
    ;;
esac

case "$OS" in
  linux)
    TARGET="${ARCH}-unknown-linux-musl"
    ;;
  darwin)
    TARGET="${ARCH}-apple-darwin"
    ;;
  *)
    echo "Error: unsupported operating system '$OS'" >&2
    exit 1
    ;;
esac

ASSET="${BINARY_NAME}-${TARGET}"
if [ "$VERSION" = "latest" ]; then
  URL="https://github.com/${REPO}/releases/latest/download/${ASSET}"
else
  URL="https://github.com/${REPO}/releases/download/${VERSION}/${ASSET}"
fi

TMP_FILE="$(mktemp)"
cleanup() {
  rm -f "$TMP_FILE"
}
trap cleanup EXIT INT TERM

echo "Downloading ${ASSET} from ${URL}"
curl -fsSL "$URL" -o "$TMP_FILE"
chmod +x "$TMP_FILE"

DEST="${INSTALL_DIR}/${BINARY_NAME}"
if [ -w "$INSTALL_DIR" ]; then
  install -m 0755 "$TMP_FILE" "$DEST"
else
  need_cmd sudo
  sudo install -m 0755 "$TMP_FILE" "$DEST"
fi

echo "Installed ${BINARY_NAME} to ${DEST}"
echo "Run '${BINARY_NAME} --version' to verify."
