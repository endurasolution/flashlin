# FlashLin production package

Everything needed to deploy FlashLin to an Ubuntu (or other systemd Linux)
server: the compiled binary, a production `server.toml`, the default web
root, a systemd unit, and an install script.

```
production/
├── bin/flashlin        Linux x86_64 release binary (glibc; built on Debian bookworm)
├── server.toml          Production config — read the comments before editing
├── flashlin.service     systemd unit
├── install.sh            Installs everything above onto this machine
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

## Before you rely on this in production

This package was built and benchmarked against nginx/Apache on
CPU-isolated Docker/Ubuntu, and the new `sendfile` fast path had a manual
security review (see `docs/PRODUCTION.md`) — but it has not been fuzzed,
soak-tested, or run under your real traffic yet. Start with a canary
instance, watch error rates and memory under load, and only then widen
the rollout. `docs/PRODUCTION.md` has the full checklist.

## Rebuilding the binary

`bin/flashlin` was cross-built for Linux (glibc, matching Debian
bookworm/Ubuntu 22.04+) from this repository's source with:

```bash
cargo build --release --locked --features acme --bin flashlin
```

To rebuild after further source changes (from the repository root, using
Docker so it works from a non-Linux machine too):

```bash
docker run --rm -v "$(pwd):/src" -w /src rust:1.95-bookworm \
  cargo build --release --locked --features acme --bin flashlin
cp target/release/flashlin production/bin/flashlin
```

Add `http3` to `--features` for the experimental HTTP/3 build (see
`docs/HTTP_PROTOCOLS.md` in the main repo); it's left out here to match
the configuration this package was actually tested with.
