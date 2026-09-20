#!/usr/bin/env bash
# CRLF self-fix: single &&-chain — avoids if/then/fi so it works even when
# bash\r shebang fails and sh/dash runs this line with \r appended.
# exec replaces the process before the trailing \r matters.
[ -z "${_FL_FIXED:-}" ] && LC_ALL=C grep -q "$(printf '\r')" "$0" 2>/dev/null && _FL_T=$(mktemp) && sed 's/\r//' "$0" > "$_FL_T" && chmod +x "$_FL_T" && exec env _FL_FIXED=1 bash "$_FL_T" "$@"
set -euo pipefail

# FlashLin production-package installer.
# Installs the binary and files already sitting in this directory (built
# from source and verified — see ../README.md) onto the machine this script
# runs on. Unlike install_linux.sh / install_flashlin_universal.sh in the
# repo root, this never downloads anything: everything it needs is already
# next to this script, so it also works with no internet access on the
# target host. Distro coverage mirrors that script:
# Supports: Debian, Ubuntu, Kali, Mint, Pop!_OS, Raspberry Pi OS
#           CentOS, RHEL, Rocky, AlmaLinux, Fedora, Oracle Linux
#           Arch, Manjaro, openSUSE Leap/Tumbleweed, Alpine, Gentoo
#
#   scp -r production user@server:/tmp/flashlin-deploy
#   ssh user@server
#   cd /tmp/flashlin-deploy && sudo ./install.sh
#
# Idempotent: safe to re-run. An existing server.toml is never overwritten —
# a new one is written alongside it as server.toml.new for you to diff.

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

INSTALL_DIR="/opt/flashlin"
BIN_PATH="/usr/local/bin/flashlin"
CONFIG_DIR="/etc/flashlin"
DOMAINS_DIR="/etc/flashlin/domains"
PUBLIC_DIR="/var/www/public"
SSL_DIR="/etc/flashlinssl/certs"
ACME_DIR="/etc/flashlin/acme"
SERVICE_FILE="/etc/systemd/system/flashlin.service"

# ── Colour helpers ──────────────────────────────────────────────────────────
green()  { echo -e "\033[32m$1\033[0m"; }
blue()   { echo -e "\033[34m$1\033[0m"; }
yellow() { echo -e "\033[33m$1\033[0m"; }
red()    { echo -e "\033[31m$1\033[0m"; }
step()   { echo; blue "=> $1"; }

# ── OS detection ─────────────────────────────────────────────────────────────
OS_ID="linux"
OS_PRETTY="Linux"

if [[ -f /etc/os-release ]]; then
  . /etc/os-release
  OS_ID="${ID:-linux}"
  OS_PRETTY="${PRETTY_NAME:-${NAME:-Linux}}"
fi

# Short badge text for the default welcome page's OS indicator.
detect_os_logo() {
  case "$OS_ID" in
    ubuntu)    echo "UB" ;;
    debian)    echo "DE" ;;
    kali)      echo "KA" ;;
    linuxmint) echo "MI" ;;
    pop)       echo "PO" ;;
    raspbian)  echo "PI" ;;
    centos)    echo "CE" ;;
    rhel)      echo "RH" ;;
    rocky)     echo "RL" ;;
    almalinux) echo "AL" ;;
    fedora)    echo "FE" ;;
    ol)        echo "OL" ;;
    arch)      echo "AR" ;;
    manjaro)   echo "MJ" ;;
    opensuse*) echo "SU" ;;
    alpine)    echo "AP" ;;
    gentoo)    echo "GE" ;;
    *)         echo "LX" ;;
  esac
}

escape_sed_value() {
  printf '%s' "$1" | sed 's/[\/&|]/\\&/g'
}

# ── Package manager detection ─────────────────────────────────────────────────
# Only used to ensure a CA bundle is present; nothing here is downloaded.
detect_pkg_install_cmd() {
  if command -v apt-get >/dev/null 2>&1; then
    apt-get update -y -qq
    echo "apt-get install -y -qq"
    return
  fi
  if command -v dnf >/dev/null 2>&1; then
    echo "dnf install -y -q"
    return
  fi
  if command -v yum >/dev/null 2>&1; then
    echo "yum install -y -q"
    return
  fi
  if command -v pacman >/dev/null 2>&1; then
    pacman -Sy --noconfirm --quiet
    echo "pacman -S --noconfirm --quiet"
    return
  fi
  if command -v zypper >/dev/null 2>&1; then
    echo "zypper install -y --no-recommends"
    return
  fi
  if command -v apk >/dev/null 2>&1; then
    apk update -q
    echo "apk add --quiet"
    return
  fi
  if command -v emerge >/dev/null 2>&1; then
    echo "emerge --ask=n"
    return
  fi
  echo ""
}

resolve_ca_pkg_name() {
  case "$OS_ID" in
    gentoo) echo "app-misc/ca-certificates" ;;
    *)      echo "ca-certificates" ;;
  esac
}

# ── Firewall ──────────────────────────────────────────────────────────────────
# TCP 80/443 serve HTTP/1.1, HTTP/2 and HTTPS; UDP 443 is for optional HTTP/3
# over QUIC (enabled via [http3] in server.toml — see docs/HTTP_PROTOCOLS.md
# in the main repo).
open_firewall_ports() {
  if command -v firewall-cmd >/dev/null 2>&1; then
    firewall-cmd --permanent --add-service=http   2>/dev/null || true
    firewall-cmd --permanent --add-service=https  2>/dev/null || true
    firewall-cmd --permanent --add-port=443/udp   2>/dev/null || true
    firewall-cmd --reload 2>/dev/null || true
    green "firewalld: HTTP, HTTPS and QUIC/HTTP3 (443/udp) allowed."
  elif command -v ufw >/dev/null 2>&1 && ufw status 2>/dev/null | grep -q "Status: active"; then
    ufw allow 80/tcp  || true
    ufw allow 443/tcp || true
    ufw allow 443/udp || true
    green "ufw: HTTP, HTTPS and QUIC/HTTP3 (443/udp) allowed."
  elif command -v nft >/dev/null 2>&1; then
    nft add rule inet filter input tcp dport 80  accept 2>/dev/null || true
    nft add rule inet filter input tcp dport 443 accept 2>/dev/null || true
    nft add rule inet filter input udp dport 443 accept 2>/dev/null || true
    green "nftables: HTTP, HTTPS and QUIC/HTTP3 (443/udp) rules added."
  elif command -v iptables >/dev/null 2>&1; then
    iptables -I INPUT -p tcp --dport 80  -j ACCEPT 2>/dev/null || true
    iptables -I INPUT -p tcp --dport 443 -j ACCEPT 2>/dev/null || true
    iptables -I INPUT -p udp --dport 443 -j ACCEPT 2>/dev/null || true
    green "iptables: HTTP, HTTPS and QUIC/HTTP3 (443/udp) allowed."
  else
    yellow "No firewall manager found (or ufw is inactive) — skipping firewall changes."
  fi
}

# ── SELinux (RHEL family) ─────────────────────────────────────────────────────
handle_selinux() {
  command -v getenforce >/dev/null 2>&1 || return 0
  local state
  state="$(getenforce)"
  [[ "$state" == "Enforcing" ]] || { yellow "SELinux is $state — no port labeling needed."; return 0; }

  if command -v semanage >/dev/null 2>&1; then
    semanage port -a -t http_port_t -p tcp 80  2>/dev/null || true
    semanage port -a -t http_port_t -p tcp 443 2>/dev/null || true
    green "SELinux: ports 80 and 443 labeled as http_port_t."
  else
    yellow "SELinux is Enforcing but 'semanage' not found."
    yellow "Install policycoreutils-python-utils and re-run, or set SELINUX=permissive in /etc/selinux/config."
  fi
}

# ── Service management ────────────────────────────────────────────────────────
# systemd is available on all modern distros; Alpine uses OpenRC as fallback.
enable_and_start_service() {
  if command -v systemctl >/dev/null 2>&1 && systemctl list-units >/dev/null 2>&1; then
    systemctl daemon-reload
    systemctl enable flashlin
    systemctl restart flashlin
  elif command -v rc-update >/dev/null 2>&1; then
    cp "$SERVICE_FILE" /etc/init.d/flashlin 2>/dev/null || true
    rc-update add flashlin default
    rc-service flashlin restart
  else
    red "Neither systemd nor OpenRC found. Start FlashLin manually:"
    echo "  $BIN_PATH $CONFIG_DIR/server.toml &"
  fi
}

show_logo() {
  echo -e "\033[36m"
  cat <<'EOF'
  ______ _           _     _     _
 |  ____| |         | |   | |   (_)
 | |__  | | __ _ ___| |__ | |    _ _ __
 |  __| | |/ _` / __| '_ \| |   | | '_ \
 | |    | | (_| \__ \ | | | |___| | | | |
 |_|    |_|\__,_|___/_| |_|_____|_|_| |_|

      Fast - Reliable - Cached - Powerful
EOF
  echo -e "\033[0m"
  green "FlashLin production-package installer"
  blue   "Detected OS: $OS_PRETTY"
  blue   "--------------------------------------------"
  echo
}

# ── Guard ─────────────────────────────────────────────────────────────────────
if [[ $EUID -ne 0 ]]; then
  red "Run as root:"
  echo "sudo ./install.sh"
  exit 1
fi

if [[ -f "$HERE/bin/flashlin" && ! -x "$HERE/bin/flashlin" ]]; then
  # Common after a zip-based transfer: zip doesn't reliably preserve the
  # Unix executable bit, so a file that arrived intact still needs this.
  yellow "bin/flashlin is present but not executable — fixing (likely lost the +x bit in transit, e.g. via a .zip)."
  chmod +x "$HERE/bin/flashlin"
fi

if [[ ! -x "$HERE/bin/flashlin" ]]; then
  red "bin/flashlin not found next to this script."
  red "Build it first — see ../README.md — or check the package upload."
  exit 1
fi

show_logo

# Architecture check — the bundled binary was built for one target; confirm
# it matches this host instead of assuming, since this package ships
# whatever was actually built (see production/README.md to rebuild for a
# different architecture).
HOST_ARCH="$(uname -m)"
BIN_ARCH="$(od -A n -j 18 -N 2 -t x1 "$HERE/bin/flashlin" 2>/dev/null | tr -d ' \n')"
case "$HOST_ARCH:$BIN_ARCH" in
  x86_64:3e00|amd64:3e00)   blue "Architecture: $HOST_ARCH (matches bundled x86-64 binary)" ;;
  aarch64:b700|arm64:b700)  blue "Architecture: $HOST_ARCH (matches bundled arm64 binary)" ;;
  *)
    yellow "Could not confirm the bundled binary matches this host's architecture"
    yellow "($HOST_ARCH detected; binary e_machine=$BIN_ARCH). Continuing — if it fails"
    yellow "to run, rebuild for this architecture (see ../README.md)." ;;
esac

step "Creating required folders"
mkdir -p "$INSTALL_DIR" "$CONFIG_DIR" "$DOMAINS_DIR" "$PUBLIC_DIR" "$SSL_DIR"

# ACME (automatic HTTPS) cache dir: must stay outside every served document
# root and private, since it holds certificate/account private keys.
mkdir -p "$ACME_DIR"
chmod 700 "$ACME_DIR"

step "Ensuring a CA certificate bundle is present"
PKG_INSTALL="$(detect_pkg_install_cmd)"
if [[ -n "$PKG_INSTALL" ]]; then
  $PKG_INSTALL "$(resolve_ca_pkg_name)" >/dev/null 2>&1 || \
    yellow "Could not confirm/install a CA bundle automatically; TLS upstreams may need one."
else
  yellow "No supported package manager found — skipping CA bundle check."
fi

step "Installing binary to $BIN_PATH"
BIN_MAGIC="$(od -A n -N 4 -t x1 "$HERE/bin/flashlin" 2>/dev/null | tr -d ' \n')"
if [[ "$BIN_MAGIC" != 7f454c46* ]]; then
  red "$HERE/bin/flashlin is not an ELF binary (magic: $BIN_MAGIC)."
  exit 1
fi
install -m 755 "$HERE/bin/flashlin" "$BIN_PATH"
"$BIN_PATH" --version

step "Writing configuration to $CONFIG_DIR"
if [[ -f "$CONFIG_DIR/server.toml" ]]; then
  cp "$HERE/server.toml" "$CONFIG_DIR/server.toml.new"
  yellow "Existing config kept: $CONFIG_DIR/server.toml"
  yellow "New default written to $CONFIG_DIR/server.toml.new — diff and merge by hand."
else
  cp "$HERE/server.toml" "$CONFIG_DIR/server.toml"
  green "Wrote $CONFIG_DIR/server.toml"
fi

step "Deploying default web root to $PUBLIC_DIR"
INDEX_WAS_ABSENT=1
[[ -f "$PUBLIC_DIR/index.html" ]] && INDEX_WAS_ABSENT=0
cp -r --update=none "$HERE/public/." "$PUBLIC_DIR/" 2>/dev/null || cp -rn "$HERE/public/." "$PUBLIC_DIR/"

if [[ $INDEX_WAS_ABSENT -eq 1 && -f "$PUBLIC_DIR/index.html" ]]; then
  # Fill in the default welcome page's OS badge and document-root path —
  # only for the page we just deployed, never touching a real site.
  OS_NAME_SAFE="$(escape_sed_value "$OS_PRETTY")"
  OS_LOGO_SAFE="$(escape_sed_value "$(detect_os_logo)")"
  PUBLIC_DIR_SAFE="$(escape_sed_value "$PUBLIC_DIR")"
  sed -i \
    -e "s|__FLASHLIN_OS_NAME__|$OS_NAME_SAFE|g" \
    -e "s|__FLASHLIN_OS_LOGO__|$OS_LOGO_SAFE|g" \
    -e "s|__FLASHLIN_DOC_ROOT__|$PUBLIC_DIR_SAFE|g" \
    "$PUBLIC_DIR/index.html"
  green "Default welcome page customized for $OS_PRETTY."
fi

step "Installing systemd service unit"
if command -v systemctl >/dev/null 2>&1; then
  mkdir -p "$(dirname "$SERVICE_FILE")"
  cp "$HERE/flashlin.service" "$SERVICE_FILE"
elif command -v rc-update >/dev/null 2>&1; then
  yellow "No systemd — OpenRC detected. Copying the unit for reference; you'll"
  yellow "need an OpenRC init script (a shell script, not this systemd unit) to"
  yellow "actually manage the service. Start FlashLin manually in the meantime:"
  yellow "  $BIN_PATH $CONFIG_DIR/server.toml &"
  cp "$HERE/flashlin.service" "$SERVICE_FILE.reference" 2>/dev/null || true
else
  yellow "No systemd or OpenRC found — skipping service unit install."
  yellow "Start FlashLin manually: $BIN_PATH $CONFIG_DIR/server.toml &"
fi

step "Validating configuration"
"$BIN_PATH" --check "$CONFIG_DIR/server.toml"

step "Opening firewall ports"
open_firewall_ports

step "Handling SELinux (RHEL/CentOS family)"
handle_selinux

step "Enabling and starting FlashLin"
enable_and_start_service

echo
blue "--------------------------------------------"
green "FlashLin installed successfully"
blue "--------------------------------------------"

echo
green "Version:"
"$BIN_PATH" --version || true

echo
green "Service status:"
if command -v systemctl >/dev/null 2>&1; then
  systemctl --no-pager --full status flashlin || true
elif command -v rc-service >/dev/null 2>&1; then
  rc-service flashlin status || true
fi

echo
yellow "Config:       $CONFIG_DIR/server.toml"
yellow "Domains:      $DOMAINS_DIR"
yellow "Website root: $PUBLIC_DIR"
yellow "SSL certs:    $SSL_DIR"
yellow "ACME cache:   $ACME_DIR (private, for automatic HTTPS)"
yellow "Binary:       $BIN_PATH"
echo
green "Run manually:"
echo "  sudo flashlin $CONFIG_DIR/server.toml"
echo
blue "Next steps:"
echo "  Validate config     - sudo flashlin --check $CONFIG_DIR/server.toml"
echo "  Hot reload          - sudo flashlin --watch $CONFIG_DIR/server.toml"
echo "  Logs                - journalctl -u flashlin -f"
echo "  TLS / ACME           - uncomment [tls] or [tls.acme] in server.toml, then: systemctl restart flashlin"
echo "  Before enabling      - read docs/PRODUCTION.md: sendfile / direct_static_io default off"
