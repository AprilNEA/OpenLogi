#!/bin/sh
#
# Cargo `runner` for Linux (wired in `.cargo/config.toml`).
#
# A Linux build has no bundle identifier to read a dev channel from, so without
# this a plain `cargo run` is a production process: it takes the installed
# agent's socket and lock, rewrites its `config.toml`, and points its systemd
# user unit at `target/`. This puts OpenLogi's own binaries on the
# `openlogi-dev` profile, the one macOS dev bundles already use. Everything
# else — tests above all, which assume the default profile — passes through.
#
#   OPENLOGI_PROFILE=prod  run a binary on the production profile anyway
#
# Cargo calls this for every binary of every `cargo run`/`test`/`bench`, so it
# stays a POSIX shell passthrough.
set -eu

case "${1##*/}" in
  openlogi | openlogi-desktop | openlogi-agent | openlogi-overlay)
    : "${OPENLOGI_PROFILE:=dev}"
    export OPENLOGI_PROFILE
    ;;
esac

exec "$@"
