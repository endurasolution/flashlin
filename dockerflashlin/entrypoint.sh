#!/bin/sh
set -e

CONFIG_FILE="${FLASHLIN_CONFIG:-/etc/flashlin/server.toml}"

if [ ! -f "$CONFIG_FILE" ]; then
    echo "[flashlin] ERROR: config file not found: $CONFIG_FILE" >&2
    exit 1
fi

if [ "${FLASHLIN_WATCH:-0}" = "1" ]; then
    exec flashlin --watch "$CONFIG_FILE"
fi
exec flashlin "$CONFIG_FILE"
