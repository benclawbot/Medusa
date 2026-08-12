#!/usr/bin/env sh
set -eu

REPO_ROOT=${1:-$(pwd)}
INSTALL_ROOT=${XDG_DATA_HOME:-$HOME/.local/share}/medusa/local-bridge
BIN_ROOT=${XDG_BIN_HOME:-$HOME/.local/bin}
TOKEN_FILE=${XDG_CONFIG_HOME:-$HOME/.config}/medusa/local-bridge-token

mkdir -p "$INSTALL_ROOT" "$BIN_ROOT" "$(dirname "$TOKEN_FILE")"
cp "$(dirname "$0")/medusa_bridge.py" "$INSTALL_ROOT/medusa_bridge.py"
chmod 700 "$INSTALL_ROOT/medusa_bridge.py"

if [ ! -s "$TOKEN_FILE" ]; then
  python3 -c 'import secrets; print(secrets.token_urlsafe(48))' > "$TOKEN_FILE"
  chmod 600 "$TOKEN_FILE"
fi

cat > "$BIN_ROOT/medusa-local-bridge" <<EOF
#!/usr/bin/env sh
exec python3 "$INSTALL_ROOT/medusa_bridge.py" \
  --repo "$REPO_ROOT" \
  --token-file "$TOKEN_FILE" \
  --allow-mutation "\$@"
EOF
chmod 700 "$BIN_ROOT/medusa-local-bridge"

printf '%s\n' "Installed: $BIN_ROOT/medusa-local-bridge"
printf '%s\n' "Token:     $TOKEN_FILE"
printf '%s\n' "Repository: $REPO_ROOT"
printf '%s\n' "Start with: $BIN_ROOT/medusa-local-bridge"
printf '%s\n' "Keep the token private and do not expose port 8765 beyond localhost."
