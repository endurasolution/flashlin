#!/usr/bin/env bash
# Installs FlashLin from this local package onto an Ubuntu/Debian (or other
# systemd Linux) server. Run this ON the target server, as root, from inside
# the uploaded production/ directory:
#
#   scp -r production user@server:/tmp/flashlin-deploy
#   ssh user@server
#   cd /tmp/flashlin-deploy && sudo ./install.sh
#
# Idempotent: safe to re-run. An existing server.toml is never overwritten —
# a new one is written alongside it as server.toml.new for you to diff.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN_PATH="/usr/local/bin/flashlin"
CONFIG_DIR="/etc/flashlin"
PUBLIC_DIR="/var/www/public"
SERVICE_FILE="/etc/systemd/system/flashlin.service"

blue()  { echo -e "\033[34m$1\033[0m"; }
green() { echo -e "\033[32m$1\033[0m"; }
step()  { echo; blue "==> $1"; }

if [[ $EUID -ne 0 ]]; then
  echo "Run as root: sudo $0" >&2
  exit 1
fi

if [[ ! -x "$HERE/bin/flashlin" ]]; then
  echo "bin/flashlin not found (or not executable) next to this script." >&2
  echo "Build it first — see ../docs/PRODUCTION.md — or check the package upload." >&2
  exit 1
fi

step "Installing binary to $BIN_PATH"
install -m 755 "$HERE/bin/flashlin" "$BIN_PATH"
"$BIN_PATH" --version

step "Writing configuration to $CONFIG_DIR"
mkdir -p "$CONFIG_DIR"
if [[ -f "$CONFIG_DIR/server.toml" ]]; then
  cp "$HERE/server.toml" "$CONFIG_DIR/server.toml.new"
  echo "Existing $CONFIG_DIR/server.toml kept."
  echo "New default written to $CONFIG_DIR/server.toml.new — diff and merge by hand."
else
  cp "$HERE/server.toml" "$CONFIG_DIR/server.toml"
  echo "Wrote $CONFIG_DIR/server.toml"
fi

step "Deploying default web root to $PUBLIC_DIR"
mkdir -p "$PUBLIC_DIR"
cp -r --update=none "$HERE/public/." "$PUBLIC_DIR/" 2>/dev/null || cp -rn "$HERE/public/." "$PUBLIC_DIR/"

step "Installing systemd unit"
cp "$HERE/flashlin.service" "$SERVICE_FILE"
systemctl daemon-reload

step "Validating configuration"
"$BIN_PATH" --check "$CONFIG_DIR/server.toml"

if command -v ufw >/dev/null 2>&1 && ufw status | grep -q "Status: active"; then
  step "Opening firewall ports (ufw is active)"
  ufw allow 80/tcp  >/dev/null || true
  ufw allow 443/tcp >/dev/null || true
fi

step "Enabling and starting the flashlin service"
systemctl enable --now flashlin

echo
green "--------------------------------------------"
green "FlashLin installed"
green "--------------------------------------------"
systemctl --no-pager --lines=0 status flashlin || true
echo
echo "Config:   $CONFIG_DIR/server.toml"
echo "Webroot:  $PUBLIC_DIR"
echo "Binary:   $BIN_PATH"
echo "Logs:     journalctl -u flashlin -f"
echo
echo "Next steps:"
echo "  - Point DNS/your webroot at $PUBLIC_DIR and edit $CONFIG_DIR/server.toml for your domain."
echo "  - For TLS, uncomment [tls] or [tls.acme] in server.toml, then: systemctl restart flashlin"
echo "  - Read ../docs/PRODUCTION.md before enabling 'sendfile' or 'direct_static_io'."
echo "  - Config changes take effect on: systemctl restart flashlin"
echo "    (or run with --watch for file-polled hot reload — see the commented line in flashlin.service)."
