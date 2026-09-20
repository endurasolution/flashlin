# FlashLin production package — universal Linux

Everything needed to deploy FlashLin to any systemd (or OpenRC) Linux
distro on x86_64: the compiled binary, a production `server.toml`, the
default web root, a systemd unit, and an install script.

Use this folder if your distro isn't Debian/Ubuntu-family or
CentOS/RHEL-family — `install.sh` auto-detects `/etc/os-release` and
handles Arch (`pacman`), openSUSE (`zypper`), Alpine (`apk` + OpenRC) and
Gentoo (`emerge`) in addition to the apt/dnf families the `debian/` and
`centos/` siblings are named after. All four `production/` subfolders ship
the identical `install.sh` — this one is just the "not sure which, or want
one package that works everywhere" option. For arm64 (Raspberry Pi and
similar), use the `raspberry-pi/` folder instead — the binary architecture
must match the target machine.

```
production/
├── bin/flashlin        Linux x86_64 release binary (glibc; built on Debian bookworm)
├── server.toml          Production config — read the comments before editing
├── flashlin.service     systemd unit
├── install.sh            Installs everything above onto this machine
├── uninstall.sh           Removes it again (see "Uninstall" below)
├── public/               Default web root (index page + error pages)
├── docs/                 PRODUCTION.md, PERFORMANCE.md, AUTOMATIC_HTTPS_AND_RELOAD.md
└── LICENSE.txt
```

**Read [docs/PRODUCTION.md](docs/PRODUCTION.md) before enabling `sendfile` or
`direct_static_io` in `server.toml`.** Both default off; this package's
`server.toml` keeps them off. That file has the checklist for turning them
on safely.

## Deploy

From your machine, upload the whole folder to the server:

```bash
scp -r production/ user@your-server:/tmp/flashlin-deploy
ssh user@your-server
cd /tmp/flashlin-deploy
sudo ./install.sh
```

`install.sh` installs the binary to `/usr/local/bin/flashlin`, writes
`/etc/flashlin/server.toml` (or saves a new one as `server.toml.new`
alongside an existing config — it never overwrites one), deploys the
default site to `/var/www/public`, installs and starts the systemd service,
and opens 80/443 in `ufw` if it's active. It's safe to re-run.

## Verify before going live

```bash
flashlin --check /etc/flashlin/server.toml   # validate config without binding a port
sudo systemctl status flashlin
curl -I http://localhost/
journalctl -u flashlin -f                     # tail logs
```

## Managing the service

Once installed, `flashlin` itself doubles as a control command — run
`flashlin --help` for the full list. These require the systemd unit
`install.sh` sets up; they're thin wrappers around `systemctl`, not a
second process supervisor:

```bash
sudo flashlin start      # systemctl start flashlin
sudo flashlin stop       # systemctl stop flashlin
sudo flashlin restart    # systemctl restart flashlin
flashlin status          # systemctl status flashlin (no sudo needed to view)
```

`flashlin purge cache [config]` is different: FlashLin's response cache
lives in the memory of the *running* server process, so nothing filesystem-
or systemd-level can clear it. This sends a loopback-only HTTP request
(`POST /flashlin/purge-cache`) to the running server itself — reachable only
from the same machine, same as `/metrics`. It reads `server.port` from the
config (default: `/etc/flashlin/server.toml`, or pass a path) to know where
to connect. Only clears the in-memory cache — never touches files on disk.

`sudo flashlin remove` stops and disables the service and removes the
binary and systemd unit — the same safe, non-destructive default as
`uninstall.sh` with no flags (config, web root and certs are left in
place). Unlike the shell script, it has no `--purge` equivalent; use
`uninstall.sh --purge` for that.

## Uninstall

```bash
sudo ./uninstall.sh            # stops the service, removes the binary and systemd unit
sudo ./uninstall.sh --purge    # also deletes config, web root, certs and ACME data (asks to confirm)
sudo ./uninstall.sh --purge -y # same, without the interactive prompt (for scripts)
```

The default run only removes what's unambiguously safe to remove — the
binary and the systemd unit — and leaves `/etc/flashlin` (your config and
any ACME account/certificates), `/etc/flashlinssl` and `/var/www/public`
in place, since they may hold real content or private keys. It prints
their paths and sizes so you can review or back them up before deciding.
`--purge` deletes those too, and always asks you to type `yes` unless you
also pass `-y`/`--yes`. Firewall rules `install.sh` opened (80/tcp,
443/tcp, 443/udp) are left as-is, in case something else on the host uses
them. Safe to re-run either form.

## Before you rely on this in production

This package was built and benchmarked against nginx/Apache on
CPU-isolated Docker/Ubuntu, and the new `sendfile` fast path had a manual
security review (see `docs/PRODUCTION.md`) — but it has not been fuzzed,
soak-tested, or run under your real traffic yet. Start with a canary
instance, watch error rates and memory under load, and only then widen
the rollout. `docs/PRODUCTION.md` has the full checklist.

## Protocols in this build

`flashlin --version` is the source of truth — it lists exactly what this
binary was compiled with. This package's binary includes HTTP/1.1, HTTP/2,
TLS/automatic certificates (ACME), and HTTP/3 over QUIC. HTTP/3 is an
**experimental** optional listener (see `docs/HTTP_PROTOCOLS.md`): it's
compiled in but does nothing until you enable both TLS (`[tls]` or
`[tls.acme]`) *and* `[http3]` in `server.toml` — QUIC requires TLS 1.3, and
`install.sh` opens UDP 443 for it, but neither is on by default. The
`[http3]` example block in `server.toml` has the exact requirements.

Functionally verified (not just `--version`): started with TLS + `[http3]`
enabled and a test certificate, the log showed
`FlashLin HTTP/3 listening (UDP / QUIC)`, and `ss` confirmed both the TCP
and UDP sockets actually bound — this checks the listener starts
correctly, not real-world QUIC interoperability with browsers/curl over a
network, which `docs/HTTP_PROTOCOLS.md` still lists as outstanding.

## Rebuilding the binary

`bin/flashlin` was cross-built for Linux (glibc, matching Debian
bookworm/Ubuntu 22.04+) from this repository's source with:

```bash
cargo build --release --locked --features acme,http3 --bin flashlin
```

To rebuild after further source changes (from the repository root, using
Docker so it works from a non-Linux machine too):

```bash
docker run --rm -v "$(pwd):/src" -w /src rust:1.95-bookworm \
  cargo build --release --locked --features acme,http3 --bin flashlin
cp target/release/flashlin production/universal/bin/flashlin
```

Drop `,http3` from `--features` for a smaller build without the
QUIC/h3/quinn dependencies if you don't need it.
