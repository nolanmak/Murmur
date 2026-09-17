#!/bin/sh
# Per-user installation; no sudo, secrets, or service enablement.
set -eu
cd "$(dirname "$0")/.."
case "$(uname -s)" in Linux) ;; *) echo 'This installer requires Linux.' >&2; exit 1;; esac
cargo build --locked --release
python3 scripts/install-linux.py install "$PWD/target/release/murmur"
