#!/bin/sh
# Fix bind-mount permissions, then drop privileges to the unprivileged appuser.
#
# 1Panel (and `docker compose` with a `./data:/data` bind mount in general)
# mounts a host directory over /data, which overrides the image's build-time
# `chown appuser /data` — the host directory is usually root-owned, so the
# unprivileged process cannot create the SQLite file and the app fails to start
# with "unable to open database file" (SQLite code 14). To handle this the
# container starts as root, fixes ownership of the data directory, then re-execs
# the app as appuser via gosu. If the container was already started as a
# non-root user (e.g. compose `user:` override), skip the chown and exec
# directly so we never error on a read-only or unwritable mount.
set -e

DATA_DIR="${DATA_DIR:-/data}"

if [ "$(id -u)" = "0" ]; then
    mkdir -p "$DATA_DIR"
    if ! chown -R appuser:appuser "$DATA_DIR" 2>/dev/null; then
        echo "docker-entrypoint: warning: chown -R appuser:appuser $DATA_DIR failed; the app may be unable to write its database" >&2
    fi
    exec gosu appuser "$@"
fi

exec "$@"
