#!/bin/sh
# Medusa installer (Unix): downloads a verified release asset, checks its
# SHA256 digest against the published SHA256SUMS, stages the binary
# atomically, records the install channel, and launches Medusa unless
# --no-launch (or MEDUSA_NO_LAUNCH=1) is given.
#
# Usage: install.sh [--channel release|main] [--no-launch]
#   MEDUSA_INSTALL_DIR  install directory (default: $HOME/.local/bin)
#   MEDUSA_CHANNEL      release|main (default: release)
#   MEDUSA_NO_LAUNCH=1  skip launching Medusa after install
set -eu

REPO="benclawbot/Medusa"
INSTALL_DIR="${MEDUSA_INSTALL_DIR:-$HOME/.local/bin}"
CHANNEL="${MEDUSA_CHANNEL:-release}"
NO_LAUNCH="${MEDUSA_NO_LAUNCH:-0}"
CHANNEL_MARKER=".medusa-install-channel"

while [ $# -gt 0 ]; do
  case "$1" in
    --channel)
      CHANNEL="${2:?--channel requires release or main}"
      shift 2
      ;;
    --channel=*)
      CHANNEL="${1#--channel=}"
      shift
      ;;
    --no-launch)
      NO_LAUNCH=1
      shift
      ;;
    -h|--help)
      printf 'Usage: install.sh [--channel release|main] [--no-launch]\n'
      exit 0
      ;;
    *)
      echo "Unknown argument: $1 (see --help)." >&2
      exit 1
      ;;
  esac
done

case "$CHANNEL" in
  release|main) ;;
  *)
    echo "Invalid channel '$CHANNEL'. Use release or main." >&2
    exit 1
    ;;
esac

# Architecture detection: select the published asset for this platform.
# Only linux/x86_64 and macOS/arm64 have published prebuilt assets today;
# anything else fails fast instead of downloading a binary that cannot run.
ARCH="$(uname -m)"
case "$(uname -s)" in
  Linux)
    case "$ARCH" in
      x86_64|amd64) ASSET="medusa-cli-linux.tar.gz" ;;
      *)
        echo "No prebuilt Medusa asset is published for Linux/$ARCH (only x86_64). Build from source instead." >&2
        exit 1
        ;;
    esac
    ;;
  Darwin)
    case "$ARCH" in
      arm64|aarch64) ASSET="medusa-cli-macos.tar.gz" ;;
      *)
        echo "No prebuilt Medusa asset is published for macOS/$ARCH (only arm64). Build from source instead." >&2
        exit 1
        ;;
    esac
    ;;
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
STAGED_BINARY=""
cleanup() {
  if [ -n "$STAGED_BINARY" ]; then
    rm -f "$STAGED_BINARY"
  fi
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT HUP INT TERM

if [ "$CHANNEL" = "main" ]; then
  # Rolling main builds are published as immutable main-<SHA> releases.
  LATEST_MAIN_TAG="$(curl --fail --silent --location "https://api.github.com/$REPO/releases?per_page=100" \
    | grep -o '"tag_name": *"main-[^"]*"' | head -n 1 | cut -d'"' -f4)"
  if [ -z "$LATEST_MAIN_TAG" ]; then
    echo "Could not find a published rolling-main release via the GitHub API." >&2
    exit 1
  fi
  DOWNLOAD_BASE="https://github.com/$REPO/releases/download/$LATEST_MAIN_TAG"
else
  DOWNLOAD_BASE="https://github.com/$REPO/releases/latest/download"
fi

ARCHIVE="$TMP_DIR/$ASSET"
URL="$DOWNLOAD_BASE/$ASSET"
DIGESTS="$TMP_DIR/SHA256SUMS"

printf 'Downloading Medusa (%s channel)...\n' "$CHANNEL"
curl --fail --location --progress-bar "$URL" --output "$ARCHIVE"
curl --fail --silent --location "$DOWNLOAD_BASE/SHA256SUMS" --output "$DIGESTS"

# Verify the archive against the published digest. Fail closed: a missing
# entry or a mismatch aborts the install before anything is staged.
EXPECTED="$(grep -F "  $ASSET" "$DIGESTS" | awk '{print $1}' | head -n 1)"
if [ -z "$EXPECTED" ]; then
  echo "The published SHA256SUMS has no entry for $ASSET; refusing to install an unverifiable download." >&2
  exit 1
fi
if command -v sha256sum >/dev/null 2>&1; then
  ACTUAL="$(sha256sum "$ARCHIVE" | awk '{print $1}')"
elif command -v shasum >/dev/null 2>&1; then
  ACTUAL="$(shasum -a 256 "$ARCHIVE" | awk '{print $1}')"
else
  echo "sha256sum or shasum is required to verify the Medusa download." >&2
  exit 1
fi
if [ "$ACTUAL" != "$EXPECTED" ]; then
  echo "Checksum mismatch for $ASSET: expected $EXPECTED, got $ACTUAL. Refusing to install." >&2
  exit 1
fi
printf 'Checksum verified.\n'

mkdir -p "$INSTALL_DIR"

# Warn loudly when this install switches the previously installed channel.
if [ -f "$INSTALL_DIR/$CHANNEL_MARKER" ]; then
  PREVIOUS="$(tr -d '[:space:]' < "$INSTALL_DIR/$CHANNEL_MARKER")"
  if [ -n "$PREVIOUS" ] && [ "$PREVIOUS" != "$CHANNEL" ]; then
    printf 'WARNING: switching install channel from %s to %s.\n' "$PREVIOUS" "$CHANNEL" >&2
    printf 'WARNING: future `medusa update` runs will warn until the channels agree again.\n' >&2
  fi
fi

tar -xzf "$ARCHIVE" -C "$TMP_DIR"
BINARY="$(find "$TMP_DIR" -type f -name medusa -perm -u+x | head -n 1)"
if [ -z "$BINARY" ]; then
  echo "The release archive did not contain an executable medusa binary." >&2
  exit 1
fi

# Stage beside the destination. This keeps the live inode untouched while the
# archive is validated and avoids ETXTBSY on Unix when Medusa is still running.
STAGED_BINARY="$(mktemp "$INSTALL_DIR/.medusa.new.XXXXXX")"
cp "$BINARY" "$STAGED_BINARY"
chmod +x "$STAGED_BINARY"
if ! "$STAGED_BINARY" --version >/dev/null 2>&1; then
  echo "The downloaded medusa binary failed validation." >&2
  exit 1
fi

# A same-directory rename publishes the complete staged inode atomically. Any
# failure before this point leaves the previous installation in place.
mv -f "$STAGED_BINARY" "$INSTALL_DIR/medusa"
STAGED_BINARY=""
printf '%s\n' "$CHANNEL" > "$INSTALL_DIR/$CHANNEL_MARKER"

case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *)
    printf '\nMedusa was installed to %s. Add that directory to PATH for future shells.\n' "$INSTALL_DIR"
    ;;
esac

printf 'Installed %s (%s channel)\n' "$("$INSTALL_DIR/medusa" --version 2>/dev/null || printf 'Medusa')" "$CHANNEL"
if [ "$NO_LAUNCH" = "1" ]; then
  exit 0
fi
printf 'Launching Medusa...\n\n'
exec "$INSTALL_DIR/medusa"
