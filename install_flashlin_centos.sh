#!/usr/bin/env bash
set -euo pipefail

FLASHLIN_ZIP_URL="https://hostfiles.b-cdn.net/flashlin/flashlin-linux.zip"
DEFAULT_PAGE_URL="https://hostfiles.b-cdn.net/flashlin/defultpage.html"

INSTALL_DIR="/opt/flashlin"
BIN_PATH="/usr/local/bin/flashlin"
CONFIG_DIR="/etc/flashlin"
DOMAINS_DIR="/etc/flashlin/domains"
PUBLIC_DIR="/var/www/public"
SSL_DIR="/etc/flashlinssl/certs"
SERVICE_FILE="/etc/systemd/system/flashlin.service"
TMP_DIR="/tmp/flashlin-install"

green()  { echo -e "\033[32m$1\033[0m"; }
blue()   { echo -e "\033[34m$1\033[0m"; }
yellow() { echo -e "\033[33m$1\033[0m"; }
red()    { echo -e "\033[31m$1\033[0m"; }

detect_os_name() {
  if [[ -f /etc/os-release ]]; then
    . /etc/os-release
    echo "${PRETTY_NAME:-${NAME:-Linux}}"
  else
    uname -s
  fi
}

detect_os_logo() {
  local os_id="linux"
  if [[ -f /etc/os-release ]]; then
    . /etc/os-release
    os_id="${ID:-linux}"
  fi

  case "$os_id" in
    centos)    echo "CE" ;;
    rhel)      echo "RH" ;;
    rocky)     echo "RL" ;;
    almalinux) echo "AL" ;;
    fedora)    echo "FE" ;;
    *)         echo "LX" ;;
  esac
}

escape_sed_value() {
  printf '%s' "$1" | sed 's/[\/&|]/\\&/g'
}

# Detect package manager: prefer dnf, fall back to yum.
detect_pkg_manager() {
  if command -v dnf >/dev/null 2>&1; then
    echo "dnf"
  elif command -v yum >/dev/null 2>&1; then
    echo "yum"
  else
    red "Neither dnf nor yum found. Is this a RHEL/CentOS system?"
    exit 1
  fi
}

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
  green "FlashLin Installer — CentOS / RHEL / Rocky / AlmaLinux"
  blue "--------------------------------------------"
  echo
}

step() {
  echo
  blue "=> $1"
}

if [[ $EUID -ne 0 ]]; then
  red "Please run as root:"
  echo "sudo bash install_flashlin_centos.sh"
  exit 1
fi

show_logo

PKG_MGR="$(detect_pkg_manager)"
blue "Package manager: $PKG_MGR"

step "Creating required folders"
mkdir -p "$INSTALL_DIR" "$CONFIG_DIR" "$DOMAINS_DIR" "$PUBLIC_DIR" "$SSL_DIR" "$TMP_DIR"

step "Installing required system tools"
"$PKG_MGR" install -y curl unzip ca-certificates

step "Downloading FlashLin package"
rm -rf "$TMP_DIR"/*
curl -L "$FLASHLIN_ZIP_URL" -o "$TMP_DIR/flashlin.zip"

step "Extracting package"
unzip -o "$TMP_DIR/flashlin.zip" -d "$TMP_DIR/package"

step "Installing FlashLin binary"
FLASHLIN_BIN="$(find "$TMP_DIR/package" -type f -name flashlin | head -n 1)"

if [[ -z "$FLASHLIN_BIN" ]]; then
  red "FlashLin binary not found inside the zip package."
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

[logging]
level = "info"
json  = false

[[routes]]
path              = "/"
root              = "/var/www/public"
index             = "index.html"
php_enabled       = false
directory_listing = false
EOF
else
  yellow "Existing config kept: $CONFIG_DIR/server.toml"
fi

step "Creating default website if missing"
if [[ ! -f "$PUBLIC_DIR/index.html" ]]; then
  OS_NAME="$(detect_os_name)"
  OS_LOGO="$(detect_os_logo)"
  PAGE_TMP="$TMP_DIR/defultpage.html"

  if curl -fsSL "$DEFAULT_PAGE_URL" -o "$PAGE_TMP"; then
    OS_NAME_SAFE="$(escape_sed_value "$OS_NAME")"
    OS_LOGO_SAFE="$(escape_sed_value "$OS_LOGO")"
    PUBLIC_DIR_SAFE="$(escape_sed_value "$PUBLIC_DIR")"

    sed -i "s|__FLASHLIN_OS_NAME__|$OS_NAME_SAFE|g" "$PAGE_TMP"
    sed -i "s|__FLASHLIN_OS_LOGO__|$OS_LOGO_SAFE|g" "$PAGE_TMP"
    sed -i "s|__FLASHLIN_DOC_ROOT__|$PUBLIC_DIR_SAFE|g" "$PAGE_TMP"

    sed -i "s|/var/www/public|$PUBLIC_DIR_SAFE|g" "$PAGE_TMP"
    sed -i "s|Ubuntu Environment Setup|$OS_NAME_SAFE Environment Setup|g" "$PAGE_TMP"

    cp "$PAGE_TMP" "$PUBLIC_DIR/index.html"
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
  <p>Installed on $(detect_os_name)</p>
</body>
</html>
EOF
  fi
else
  yellow "Existing website kept: $PUBLIC_DIR/index.html"
fi

step "Creating systemd service"
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
if command -v firewall-cmd >/dev/null 2>&1; then
  firewall-cmd --permanent --add-service=http  || true
  firewall-cmd --permanent --add-service=https || true
  firewall-cmd --reload || true
  green "firewalld: HTTP and HTTPS allowed."
elif command -v ufw >/dev/null 2>&1; then
  ufw allow 80/tcp  || true
  ufw allow 443/tcp || true
else
  yellow "No firewall manager found, skipping firewall changes."
fi

step "Disabling SELinux enforcement for port binding (if active)"
if command -v getenforce >/dev/null 2>&1; then
  SELINUX_STATE="$(getenforce)"
  if [[ "$SELINUX_STATE" == "Enforcing" ]]; then
    if command -v semanage >/dev/null 2>&1; then
      semanage port -a -t http_port_t -p tcp 80  2>/dev/null || true
      semanage port -a -t http_port_t -p tcp 443 2>/dev/null || true
      green "SELinux: ports 80 and 443 labeled as http_port_t."
    else
      yellow "SELinux is Enforcing but 'semanage' not found."
      yellow "Install policycoreutils-python-utils and re-run, or set SELinux to permissive."
    fi
  else
    yellow "SELinux is $SELINUX_STATE — no port labeling needed."
  fi
fi

step "Enabling and starting FlashLin"
systemctl daemon-reload
systemctl enable flashlin
systemctl restart flashlin

echo
blue "--------------------------------------------"
green "FlashLin installed successfully"
blue "--------------------------------------------"

echo
green "Version:"
"$BIN_PATH" --version || true

echo
green "Service status:"
systemctl --no-pager --full status flashlin || true

echo
yellow "Config:       $CONFIG_DIR/server.toml"
yellow "Domains:      $DOMAINS_DIR"
yellow "Website root: $PUBLIC_DIR"
yellow "SSL certs:    $SSL_DIR"
yellow "Binary:       $BIN_PATH"
echo
green "Run manually:"
echo "sudo flashlin /etc/flashlin/server.toml"
