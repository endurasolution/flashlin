#!/usr/bin/env bash
# CRLF self-fix: single &&-chain — avoids if/then/fi so it works even when
# bash\r shebang fails and sh/dash runs this line with \r appended.
# exec replaces the process before the trailing \r matters.
[ -z "${_FL_FIXED:-}" ] && LC_ALL=C grep -q "$(printf '\r')" "$0" 2>/dev/null && _FL_T=$(mktemp) && sed 's/\r//' "$0" > "$_FL_T" && chmod +x "$_FL_T" && exec env _FL_FIXED=1 bash "$_FL_T" "$@"
set -euo pipefail

# Universal FlashLin installer.
# Supports: Debian, Ubuntu, Kali, Mint, Pop!_OS, Raspberry Pi OS
#           CentOS, RHEL, Rocky, AlmaLinux, Fedora, Oracle Linux
#           Arch, Manjaro, openSUSE Leap/Tumbleweed, Alpine, Gentoo

FLASHLIN_PKG_URL="https://hostfiles.b-cdn.net/flashlin/flashlin-linux.tar.gz"
DEFAULT_PAGE_URL="https://hostfiles.b-cdn.net/flashlin/defultpage.html"

INSTALL_DIR="/opt/flashlin"
BIN_PATH="/usr/local/bin/flashlin"
CONFIG_DIR="/etc/flashlin"
DOMAINS_DIR="/etc/flashlin/domains"
PUBLIC_DIR="/var/www/public"
SSL_DIR="/etc/flashlinssl/certs"
ACME_DIR="/etc/flashlin/acme"
SERVICE_FILE="/etc/systemd/system/flashlin.service"
TMP_DIR="/tmp/flashlin-install"

# ── Colour helpers ──────────────────────────────────────────────────────────
green()  { echo -e "\033[32m$1\033[0m"; }
blue()   { echo -e "\033[34m$1\033[0m"; }
yellow() { echo -e "\033[33m$1\033[0m"; }
red()    { echo -e "\033[31m$1\033[0m"; }
step()   { echo; blue "=> $1"; }

# ── OS detection ─────────────────────────────────────────────────────────────
OS_ID="linux"
OS_ID_LIKE=""
OS_PRETTY="Linux"

if [[ -f /etc/os-release ]]; then
  . /etc/os-release
  OS_ID="${ID:-linux}"
  OS_ID_LIKE="${ID_LIKE:-}"
  OS_PRETTY="${PRETTY_NAME:-${NAME:-Linux}}"
fi

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
# Returns the install command prefix (manager + flags) ready to append packages.
detect_pkg_install_cmd() {
  # Debian family
  if command -v apt-get >/dev/null 2>&1; then
    apt-get update -y -qq
    echo "apt-get install -y -qq"
    return
  fi
  # RHEL/CentOS family — prefer dnf
  if command -v dnf >/dev/null 2>&1; then
    echo "dnf install -y -q"
    return
  fi
  if command -v yum >/dev/null 2>&1; then
    echo "yum install -y -q"
    return
  fi
  # Arch family
  if command -v pacman >/dev/null 2>&1; then
    pacman -Sy --noconfirm --quiet
    echo "pacman -S --noconfirm --quiet"
    return
  fi
  # openSUSE
  if command -v zypper >/dev/null 2>&1; then
    echo "zypper install -y --no-recommends"
    return
  fi
  # Alpine
  if command -v apk >/dev/null 2>&1; then
    apk update -q
    echo "apk add --quiet"
    return
  fi
  # Gentoo
  if command -v emerge >/dev/null 2>&1; then
    echo "emerge --ask=n"
    return
  fi
  red "No supported package manager found."
  exit 1
}

# Map generic package names to distro-specific names where they differ.
resolve_pkg_name() {
  local pkg="$1"
  case "$OS_ID" in
    ubuntu|debian|kali|linuxmint|pop|raspbian)
      case "$pkg" in
        7zip) echo "p7zip-full" ;;
        *)    echo "$pkg" ;;
      esac ;;
    centos|rhel|rocky|almalinux|fedora|ol)
      case "$pkg" in
        7zip) echo "p7zip p7zip-plugins" ;;
        *)    echo "$pkg" ;;
      esac ;;
    arch|manjaro)
      case "$pkg" in
        7zip)            echo "p7zip" ;;
        ca-certificates) echo "ca-certificates" ;;
        *)               echo "$pkg" ;;
      esac ;;
    opensuse*|sles)
      case "$pkg" in
        7zip)            echo "p7zip" ;;
        ca-certificates) echo "ca-certificates" ;;
        *)               echo "$pkg" ;;
      esac ;;
    alpine)
      case "$pkg" in
        7zip)            echo "p7zip" ;;
        ca-certificates) echo "ca-certificates" ;;
        unzip)           echo "unzip" ;;
        curl)            echo "curl" ;;
        *)               echo "$pkg" ;;
      esac ;;
    gentoo)
      case "$pkg" in
        7zip) echo "app-arch/p7zip" ;;
        *)    echo "$pkg" ;;
      esac ;;
    *)
      case "$pkg" in
        7zip) echo "p7zip-full" ;;
        *)    echo "$pkg" ;;
      esac ;;
  esac
}

# ── Firewall ──────────────────────────────────────────────────────────────────
# TCP 443 serves HTTP/1.1, HTTP/2 and HTTPS; UDP 443 is opened for optional
# HTTP/3 over QUIC (enabled via [http3] in server.toml — see docs/HTTP_PROTOCOLS.md).
open_firewall_ports() {
  if command -v firewall-cmd >/dev/null 2>&1; then
    firewall-cmd --permanent --add-service=http   2>/dev/null || true
    firewall-cmd --permanent --add-service=https  2>/dev/null || true
    firewall-cmd --permanent --add-port=443/udp   2>/dev/null || true
    firewall-cmd --reload 2>/dev/null || true
    green "firewalld: HTTP, HTTPS and QUIC/HTTP3 (443/udp) allowed."
  elif command -v ufw >/dev/null 2>&1; then
    ufw allow 80/tcp  || true
    ufw allow 443/tcp || true
    ufw allow 443/udp || true
    green "ufw: HTTP, HTTPS and QUIC/HTTP3 (443/udp) allowed."
  elif command -v nft >/dev/null 2>&1; then
    # Minimal nftables rules for systems without a manager
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
    yellow "No firewall manager found — skipping firewall changes."
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
    # Alpine OpenRC
    cp "$SERVICE_FILE" /etc/init.d/flashlin 2>/dev/null || true
    rc-update add flashlin default
    rc-service flashlin restart
  else
    red "Neither systemd nor OpenRC found. Start FlashLin manually:"
    echo "  $BIN_PATH $CONFIG_DIR/server.toml &"
  fi
}

# ── Logo ──────────────────────────────────────────────────────────────────────
show_logo() {
  clear
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
  green "FlashLin Universal Installer"
  blue   "Detected OS: $OS_PRETTY"
  blue   "--------------------------------------------"
  echo
}

# ── Guard ─────────────────────────────────────────────────────────────────────
if [[ $EUID -ne 0 ]]; then
  red "Please run as root:"
  echo "sudo bash install_flashlin_universal.sh"
  exit 1
fi

# Architecture check — only x86_64 and arm64 are shipped.
ARCH="$(uname -m)"
case "$ARCH" in
  x86_64|amd64|aarch64|arm64) ;;
  *)
    red "Unsupported architecture: $ARCH"
    red "FlashLin ships binaries for x86_64 and arm64 only."
    exit 1
    ;;
esac

show_logo
blue "Architecture: $ARCH"

# ── Install ───────────────────────────────────────────────────────────────────
step "Creating required folders"
mkdir -p "$INSTALL_DIR" "$CONFIG_DIR" "$DOMAINS_DIR" "$PUBLIC_DIR" "$SSL_DIR" "$TMP_DIR"

# ACME (automatic HTTPS) cache dir: must stay outside every served document
# root and private, since it holds certificate/account private keys.
mkdir -p "$ACME_DIR"
chmod 700 "$ACME_DIR"

step "Installing required system tools"
PKG_INSTALL="$(detect_pkg_install_cmd)"
CURL_PKG="$(resolve_pkg_name curl)"
CA_PKG="$(resolve_pkg_name ca-certificates)"
$PKG_INSTALL "$CURL_PKG" "$CA_PKG"

step "Downloading FlashLin package"
rm -rf "$TMP_DIR"/*
PKG_PATH="$TMP_DIR/flashlin.tar.gz"

# -fSL: fail on HTTP errors, show errors, follow redirects
if ! curl -fSL --progress-bar "$FLASHLIN_PKG_URL" -o "$PKG_PATH"; then
  red "Download failed (HTTP error or network issue)."
  red "URL: $FLASHLIN_PKG_URL"
  exit 1
fi

if [[ ! -s "$PKG_PATH" ]]; then
  red "Download produced an empty file."
  red "URL: $FLASHLIN_PKG_URL"
  exit 1
fi

green "Downloaded $(du -sh "$PKG_PATH" | cut -f1)."

step "Extracting package"
mkdir -p "$TMP_DIR/package"

# Read first 2 bytes to confirm gzip magic (1f 8b).
MAGIC="$(od -A n -N 2 -t x1 "$PKG_PATH" 2>/dev/null | tr -d ' \n')"
green "File magic bytes: $MAGIC"

if [[ "$MAGIC" == 1f8b* ]]; then
  green "Detected: gzip/tar archive"
  tar -xzf "$PKG_PATH" -C "$TMP_DIR/package"

elif [[ "$MAGIC" == 425a* ]]; then
  green "Detected: bzip2/tar archive"
  tar -xjf "$PKG_PATH" -C "$TMP_DIR/package"

elif [[ "$MAGIC" == fd37* ]]; then
  green "Detected: xz/tar archive"
  tar -xJf "$PKG_PATH" -C "$TMP_DIR/package"

elif [[ "$MAGIC" == 504b* ]]; then
  green "Detected: ZIP archive — extracting with 7z"
  if command -v 7z >/dev/null 2>&1; then
    7z x "$PKG_PATH" -o"$TMP_DIR/package" -y
  elif command -v 7za >/dev/null 2>&1; then
    7za x "$PKG_PATH" -o"$TMP_DIR/package" -y
  else
    red "7z not found. Install p7zip-full and re-run."
    exit 1
  fi

elif [[ "$MAGIC" == 7f45* ]]; then
  green "Detected: raw ELF binary — using directly"
  cp "$PKG_PATH" "$TMP_DIR/package/flashlin"

else
  red "Unrecognised file format (magic: $MAGIC)."
  red "URL: $FLASHLIN_PKG_URL"
  red "Debug: od -A n -N 4 -t x1 $PKG_PATH"
  exit 1
fi

step "Installing FlashLin binary"
# Exclude flashlin.d (Makefile dependency file) and find the ELF binary.
FLASHLIN_BIN="$(find "$TMP_DIR/package" -type f \( -name flashlin -o -name flashlin-linux \) ! -name "*.d" | head -n 1)"

if [[ -z "$FLASHLIN_BIN" ]]; then
  red "FlashLin binary not found inside the package."
  red "Contents of extracted package:"
  find "$TMP_DIR/package" -maxdepth 3 -type f | head -30 || true
  exit 1
fi

# Verify it is actually an ELF executable before installing.
BIN_MAGIC="$(od -A n -N 2 -t x1 "$FLASHLIN_BIN" 2>/dev/null | tr -d ' \n')"
if [[ "$BIN_MAGIC" != 7f45* ]]; then
  red "Found file '$FLASHLIN_BIN' but it is not an ELF binary (magic: $BIN_MAGIC)."
  exit 1
fi

cp "$FLASHLIN_BIN" "$BIN_PATH"
chmod 755 "$BIN_PATH"

step "Creating default server.toml if missing"
if [[ ! -f "$CONFIG_DIR/server.toml" ]]; then
  cat > "$CONFIG_DIR/server.toml" <<'EOF'
[server]
host            = "0.0.0.0"
port            = 80
server_name     = "FlashLin"
root            = "/var/www/public"
index           = ["index.php", "index.html"]
max_connections = 1000
domains_dir     = "./domains"

[security]
max_body_size          = "20MB"
max_header_size        = "32KB"
hide_server_header     = true
block_dotfiles         = true
block_sensitive_files  = true
security_headers       = true

[security.rate_limit]
limit = "200/min"
burst = 50

[compression]
gzip     = true
brotli   = true
min_size = 1024

[cache]
enabled       = true
max_size      = "128MB"
ttl           = "300s"
max_file_size = "10MB"

[logging]
level = "info"
json  = false

[[routes]]
path              = "/"
root              = "/var/www/public"
index             = "index.html"
php_enabled       = false
directory_listing = false

# Optional HTTP/3 over QUIC (needs TLS certs and an open UDP 443; see
# docs/HTTP_PROTOCOLS.md and examples/http3.toml):
# [http3]
# enabled = true
# port    = 443

# Optional automatic HTTPS via Let's Encrypt (needs a binary built with the
# `acme` feature; see docs/AUTOMATIC_HTTPS_AND_RELOAD.md and
# examples/automatic-https.toml):
# [tls.acme]
# domains       = ["example.com", "www.example.com"]
# email         = "admin@example.com"
# cache_dir     = "/etc/flashlin/acme"
# production    = false
# accept_terms  = false
EOF
else
  yellow "Existing config kept: $CONFIG_DIR/server.toml"
fi

step "Deploying default website to public folder"
OS_NAME="$OS_PRETTY"
OS_LOGO="$(detect_os_logo)"
PAGE_TMP="$TMP_DIR/defultpage.html"

if curl -fsSL "$DEFAULT_PAGE_URL" -o "$PAGE_TMP"; then
  OS_NAME_SAFE="$(escape_sed_value "$OS_NAME")"
  OS_LOGO_SAFE="$(escape_sed_value "$OS_LOGO")"
  PUBLIC_DIR_SAFE="$(escape_sed_value "$PUBLIC_DIR")"

  sed -i "s|__FLASHLIN_OS_NAME__|$OS_NAME_SAFE|g" "$PAGE_TMP"
  sed -i "s|__FLASHLIN_OS_LOGO__|$OS_LOGO_SAFE|g" "$PAGE_TMP"
  sed -i "s|__FLASHLIN_DOC_ROOT__|$PUBLIC_DIR_SAFE|g" "$PAGE_TMP"

  # Fallback replacements for older CDN page templates.
  sed -i "s|/var/www/public|$PUBLIC_DIR_SAFE|g" "$PAGE_TMP"
  sed -i "s|Ubuntu Environment Setup|$OS_NAME_SAFE Environment Setup|g" "$PAGE_TMP"

  cp "$PAGE_TMP" "$PUBLIC_DIR/index.html"
  green "Default page deployed to $PUBLIC_DIR/index.html"
else
  yellow "Default page download failed, creating a simple fallback page."
  cat > "$PUBLIC_DIR/index.html" <<EOF
<!doctype html>
<html>
<head>
  <meta charset="utf-8">
  <title>FlashLin</title>
</head>
<body>
  <h1>FlashLin is running</h1>
  <p>Installed on $OS_NAME</p>
</body>
</html>
EOF
fi

step "Creating systemd service unit"
cat > "$SERVICE_FILE" <<EOF
[Unit]
Description=FlashLin Web Server
After=network.target

[Service]
Type=simple
ExecStart=$BIN_PATH $CONFIG_DIR/server.toml
Restart=always
RestartSec=3
LimitNOFILE=65535

[Install]
WantedBy=multi-user.target
EOF

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
echo "sudo flashlin /etc/flashlin/server.toml"
echo
blue "Optional v2 features (see README.md and docs/):"
echo "  HTTP/3 (QUIC)     - UDP 443 is now open; enable [http3] in server.toml"
echo "  Automatic HTTPS   - enable [tls.acme] in server.toml (ACME cache: $ACME_DIR)"
echo "  Validate config   - sudo flashlin --check $CONFIG_DIR/server.toml"
echo "  Hot reload        - sudo flashlin --watch $CONFIG_DIR/server.toml"
