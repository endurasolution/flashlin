#!/usr/bin/env bash
# CRLF self-fix: single &&-chain — avoids if/then/fi so it works even when
# bash\r shebang fails and sh/dash runs this line with \r appended.
# exec replaces the process before the trailing \r matters.
[ -z "${_FL_FIXED:-}" ] && LC_ALL=C grep -q "$(printf '\r')" "$0" 2>/dev/null && _FL_T=$(mktemp) && sed 's/\r//' "$0" > "$_FL_T" && chmod +x "$_FL_T" && exec env _FL_FIXED=1 bash "$_FL_T" "$@"
set -euo pipefail

# FlashLin CDN installer — detects distro + architecture, downloads the
# matching production package (a zip built from production/<name>/ — see
# production/README.md), extracts it, and runs its bundled install.sh.
#
# Supports: Debian, Ubuntu, Kali, Mint, Pop!_OS, Raspberry Pi OS
#           CentOS, RHEL, Rocky, AlmaLinux, Fedora, Oracle Linux
#           Arch, Manjaro, openSUSE Leap/Tumbleweed, Alpine, Gentoo
# Architectures: x86_64 (all of the above) and arm64/aarch64 (Raspberry Pi
# package — the only arm64 build currently shipped).
#
# ── FILL THESE IN once the five zips are uploaded to BunnyCDN ──────────────
# Each must point at the matching zip in production/ (flashlin-*.zip).
FLASHLIN_URL_UBUNTU="https://REPLACE_ME.b-cdn.net/flashlin/flashlin-ubuntu-x86_64.zip"
FLASHLIN_URL_DEBIAN="https://REPLACE_ME.b-cdn.net/flashlin/flashlin-debian-x86_64.zip"
FLASHLIN_URL_CENTOS="https://REPLACE_ME.b-cdn.net/flashlin/flashlin-centos-x86_64.zip"
FLASHLIN_URL_UNIVERSAL="https://REPLACE_ME.b-cdn.net/flashlin/flashlin-universal-x86_64.zip"
FLASHLIN_URL_RASPBERRY_PI="https://REPLACE_ME.b-cdn.net/flashlin/flashlin-raspberry-pi-arm64.zip"
# ─────────────────────────────────────────────────────────────────────────

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

# ── Package manager detection ─────────────────────────────────────────────────
# Returns the install command prefix (manager + flags) ready to append packages.
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
  red "No supported package manager found."
  exit 1
}

# ── Logo ──────────────────────────────────────────────────────────────────────
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
  green "FlashLin CDN installer"
  blue   "Detected OS: $OS_PRETTY"
  blue   "--------------------------------------------"
  echo
}

# ── Guard ─────────────────────────────────────────────────────────────────────
if [[ $EUID -ne 0 ]]; then
  red "Please run as root:"
  echo "sudo bash install_linux_cdn.sh"
  exit 1
fi

# ── Architecture + package selection ──────────────────────────────────────────
# Only x86_64 and arm64 are shipped. arm64 always gets the Raspberry Pi
# package — it's the only arm64 build, regardless of distro. x86_64 picks
# the package whose README best matches the detected distro family; they
# are functionally identical (same install.sh, same binary build), so this
# is about matching docs/branding, not a functional requirement.
ARCH="$(uname -m)"
case "$ARCH" in
  x86_64|amd64)
    NORM_ARCH="x86_64"
    case "$OS_ID" in
      ubuntu) PKG_URL="$FLASHLIN_URL_UBUNTU" ;;
      debian|kali|linuxmint|pop|raspbian) PKG_URL="$FLASHLIN_URL_DEBIAN" ;;
      centos|rhel|rocky|almalinux|fedora|ol) PKG_URL="$FLASHLIN_URL_CENTOS" ;;
      *)
        case "$OS_ID_LIKE" in
          *debian*) PKG_URL="$FLASHLIN_URL_DEBIAN" ;;
          *rhel*|*fedora*) PKG_URL="$FLASHLIN_URL_CENTOS" ;;
          *) PKG_URL="$FLASHLIN_URL_UNIVERSAL" ;;
        esac
        ;;
    esac
    ;;
  aarch64|arm64)
    NORM_ARCH="arm64"
    PKG_URL="$FLASHLIN_URL_RASPBERRY_PI"
    ;;
  *)
    red "Unsupported architecture: $ARCH"
    red "FlashLin ships binaries for x86_64 and arm64 only."
    exit 1
    ;;
esac

if [[ "$PKG_URL" == *REPLACE_ME* ]]; then
  red "This installer's BunnyCDN URLs haven't been filled in yet."
  red "Edit the FLASHLIN_URL_* constants near the top of this script."
  exit 1
fi

show_logo
blue "Architecture: $ARCH ($NORM_ARCH)"
blue "Package:      $PKG_URL"

# ── Install prerequisites ─────────────────────────────────────────────────────
step "Installing required system tools"
PKG_INSTALL="$(detect_pkg_install_cmd)"
# Only install curl if it's missing — on RHEL/CentOS-family images it often
# ships pre-installed as curl-minimal, and `dnf install curl` then fails with
# a package conflict instead of a no-op.
MISSING_PKGS=()
command -v curl  >/dev/null 2>&1 || MISSING_PKGS+=(curl)
command -v unzip >/dev/null 2>&1 || MISSING_PKGS+=(unzip)
if [[ ${#MISSING_PKGS[@]} -gt 0 ]]; then
  $PKG_INSTALL "${MISSING_PKGS[@]}"
fi
$PKG_INSTALL ca-certificates >/dev/null 2>&1 || true

# ── Download ──────────────────────────────────────────────────────────────────
step "Downloading FlashLin package"
rm -rf "$TMP_DIR"
mkdir -p "$TMP_DIR"
PKG_PATH="$TMP_DIR/flashlin-package.zip"

if ! curl -fSL --progress-bar "$PKG_URL" -o "$PKG_PATH"; then
  red "Download failed (HTTP error or network issue)."
  red "URL: $PKG_URL"
  exit 1
fi

if [[ ! -s "$PKG_PATH" ]]; then
  red "Download produced an empty file."
  red "URL: $PKG_URL"
  exit 1
fi

green "Downloaded $(du -sh "$PKG_PATH" | cut -f1)."

# ── Extract ───────────────────────────────────────────────────────────────────
step "Extracting package"
EXTRACT_DIR="$TMP_DIR/package"
mkdir -p "$EXTRACT_DIR"
unzip -q -o "$PKG_PATH" -d "$EXTRACT_DIR"

# The zip wraps everything in a single top-level folder (e.g. flashlin-ubuntu/).
PKG_ROOT="$(find "$EXTRACT_DIR" -mindepth 1 -maxdepth 1 -type d | head -n 1)"
if [[ -z "$PKG_ROOT" || ! -f "$PKG_ROOT/install.sh" ]]; then
  red "Extracted package doesn't look right — install.sh not found."
  red "Contents:"
  find "$EXTRACT_DIR" -maxdepth 3 | head -30
  exit 1
fi

# ── Hand off to the package's own installer ───────────────────────────────────
# Everything past this point (config, systemd unit, firewall, SELinux, the
# default web root, service start) is handled by the bundled install.sh —
# the exact same script shipped and tested in production/<distro>/install.sh.
step "Running the package installer"
chmod +x "$PKG_ROOT/install.sh" "$PKG_ROOT/uninstall.sh" "$PKG_ROOT/bin/flashlin" 2>/dev/null || true
cd "$PKG_ROOT"
exec ./install.sh
