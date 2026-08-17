#!/bin/sh
set -eu

REPO="benclawbot/Medusa"
INSTALL_DIR="${MEDUSA_INSTALL_DIR:-$HOME/.local/bin}"

case "$(uname -s)" in
  Linux) ASSET="medusa-cli-linux.tar.gz" ;;
  Darwin) ASSET="medusa-cli-macos.tar.gz" ;;
  *)
    echo "Unsupported platform. On Windows, use install.ps1." >&2
    exit 1
    ;;
esac

if ! command -v curl >/dev/null 2>&1; then
  echo "curl is required to install Medusa." >&2
  exit 1
fi

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT HUP INT TERM
ARCHIVE="$TMP_DIR/$ASSET"
URL="https://github.com/$REPO/releases/latest/download/$ASSET"

printf 'Downloading Medusa...\n'
curl --fail --location --progress-bar "$URL" --output "$ARCHIVE"

mkdir -p "$INSTALL_DIR"
tar -xzf "$ARCHIVE" -C "$TMP_DIR"
BINARY="$(find "$TMP_DIR" -type f -name medusa -perm -u+x | head -n 1)"
if [ -z "$BINARY" ]; then
  BINARY="$(find "$TMP_DIR" -type f -name medusa | head -n 1)"
fi
if [ -z "$BINARY" ]; then
  echo "The release archive did not contain the medusa binary." >&2
  exit 1
fi

cp "$BINARY" "$INSTALL_DIR/medusa"
chmod +x "$INSTALL_DIR/medusa"

case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *)
    printf '\nMedusa was installed to %s. Add that directory to PATH for future shells.\n' "$INSTALL_DIR"
    ;;
esac

printf 'Installed %s\nLaunching Medusa...\n\n' "$("$INSTALL_DIR/medusa" --version 2>/dev/null || printf 'Medusa')"
exec "$INSTALL_DIR/medusa"
