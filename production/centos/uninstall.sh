#!/usr/bin/env bash
# CRLF self-fix — see install.sh for why this line exists as-is.
[ -z "${_FL_FIXED:-}" ] && LC_ALL=C grep -q "$(printf '\r')" "$0" 2>/dev/null && _FL_T=$(mktemp) && sed 's/\r//' "$0" > "$_FL_T" && chmod +x "$_FL_T" && exec env _FL_FIXED=1 bash "$_FL_T" "$@"
set -euo pipefail

# Removes what production/install.sh installed.
#
#   sudo ./uninstall.sh            Stops the service and removes the binary
#                                   and systemd unit. Your config, web root,
#                                   TLS certs and ACME account are left in
#                                   place and just reported, since they may
#                                   hold real content or keys.
#   sudo ./uninstall.sh --purge    Also deletes /etc/flashlin (config,
#                                   domains, ACME cache), /etc/flashlinssl
#                                   and /var/www/public. Destructive and
#                                   asks for interactive confirmation unless
#                                   you also pass -y/--yes.
#
# Never touches anything install.sh didn't create; safe to re-run.

BIN_PATH="/usr/local/bin/flashlin"
CONFIG_DIR="/etc/flashlin"
PUBLIC_DIR="/var/www/public"
SSL_DIR="/etc/flashlinssl"
SERVICE_FILE="/etc/systemd/system/flashlin.service"
INSTALL_DIR="/opt/flashlin"

green()  { echo -e "\033[32m$1\033[0m"; }
blue()   { echo -e "\033[34m$1\033[0m"; }
yellow() { echo -e "\033[33m$1\033[0m"; }
red()    { echo -e "\033[31m$1\033[0m"; }
step()   { echo; blue "=> $1"; }

PURGE=0
ASSUME_YES=0
for arg in "$@"; do
  case "$arg" in
    --purge) PURGE=1 ;;
    -y|--yes) ASSUME_YES=1 ;;
    -h|--help)
      sed -n '6,19p' "$0" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      red "Unknown argument: $arg (use --purge, -y/--yes, or -h/--help)"
      exit 1
      ;;
  esac
done

if [[ $EUID -ne 0 ]]; then
  red "Run as root: sudo $0 ${*:-}"
  exit 1
fi

step "Stopping and disabling the flashlin service"
if command -v systemctl >/dev/null 2>&1 && systemctl list-units >/dev/null 2>&1; then
  systemctl stop flashlin 2>/dev/null || true
  systemctl disable flashlin 2>/dev/null || true
elif command -v rc-service >/dev/null 2>&1; then
  rc-service flashlin stop 2>/dev/null || true
  command -v rc-update >/dev/null 2>&1 && rc-update del flashlin default 2>/dev/null || true
else
  # No service manager found; if flashlin is still running manually, this
  # won't touch it — stop it yourself before (or after) removing the binary.
  yellow "No systemd/OpenRC found; if flashlin is running manually, stop it yourself."
fi

step "Removing the systemd service unit"
if [[ -f "$SERVICE_FILE" ]]; then
  rm -f "$SERVICE_FILE"
  command -v systemctl >/dev/null 2>&1 && systemctl daemon-reload 2>/dev/null || true
  green "Removed $SERVICE_FILE"
else
  yellow "$SERVICE_FILE not present; nothing to remove."
fi
rm -f /etc/init.d/flashlin "$SERVICE_FILE.reference" 2>/dev/null || true

step "Removing the binary"
if [[ -f "$BIN_PATH" ]]; then
  rm -f "$BIN_PATH"
  green "Removed $BIN_PATH"
else
  yellow "$BIN_PATH not present; nothing to remove."
fi

[[ -d "$INSTALL_DIR" ]] && rmdir --ignore-fail-on-non-empty "$INSTALL_DIR" 2>/dev/null || true

# ── Data that may hold real content: config, web root, certs, ACME account ──
DATA_PATHS=()
[[ -e "$CONFIG_DIR" ]] && DATA_PATHS+=("$CONFIG_DIR")
[[ -e "$SSL_DIR" ]] && DATA_PATHS+=("$SSL_DIR")
[[ -e "$PUBLIC_DIR" ]] && DATA_PATHS+=("$PUBLIC_DIR")

if [[ ${#DATA_PATHS[@]} -eq 0 ]]; then
  step "No leftover config, web root or certs found"
elif [[ $PURGE -eq 0 ]]; then
  step "Left in place (contains your config, site content, or keys)"
  for p in "${DATA_PATHS[@]}"; do
    echo "  $p ($(du -sh "$p" 2>/dev/null | cut -f1))"
  done
  echo
  yellow "Re-run with --purge to also delete these. This is permanent — it"
  yellow "includes your server.toml, any TLS private keys, and your ACME"
  yellow "account/certificates if automatic HTTPS was ever enabled."
else
  step "About to permanently delete:"
  for p in "${DATA_PATHS[@]}"; do
    echo "  $p ($(du -sh "$p" 2>/dev/null | cut -f1))"
  done
  if [[ $ASSUME_YES -eq 0 ]]; then
    echo
    read -r -p "Type 'yes' to permanently delete the above: " CONFIRM
    if [[ "$CONFIRM" != "yes" ]]; then
      yellow "Not confirmed — leaving these in place. Binary and service are still removed."
      CONFIRM=""
    fi
  else
    CONFIRM="yes"
  fi
  if [[ "${CONFIRM:-}" == "yes" ]]; then
    for p in "${DATA_PATHS[@]}"; do
      rm -rf "$p"
      green "Removed $p"
    done
  fi
fi

echo
green "--------------------------------------------"
green "FlashLin service, binary and systemd unit removed."
green "--------------------------------------------"
if [[ $PURGE -eq 0 ]]; then
  echo "Firewall rules (80/tcp, 443/tcp, 443/udp) opened by install.sh were left"
  echo "as-is, in case something else on this host uses them."
fi
