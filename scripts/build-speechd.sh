#!/bin/sh
# Build the fotw-speechd helper (Parakeet Unified, Silero VAD; Murmur #101)
# from the FlyOnTheWall revision Cargo.lock pins, and print its path.
#
# Needs Swift 6.2+ and FOTW_FLUIDAUDIO_SOURCE, a FluidAudio 0.17.3 checkout at
# a6b826ce9a09028fc685a4b32a8927ec920c3188 (verified before building). Nothing
# is cloned or downloaded except FluidAudio's own SwiftPM binary artifact.
#
# MURMUR_SPEECHD_SOURCE=<FlyOnTheWall checkout> builds from that checkout
# instead, for local work before the helper reaches the pinned revision. A
# bundle built that way is for local measurement only.
set -eu
cd "$(dirname "$0")/.."
fail() { printf 'error: %s\n' "$@" >&2; exit 1; }
[ "$(uname -s)" = Darwin ] || fail "fotw-speechd is macOS-only"
fluid="${FOTW_FLUIDAUDIO_SOURCE:-}"
[ -n "$fluid" ] || fail "set FOTW_FLUIDAUDIO_SOURCE to a FluidAudio 0.17.3 checkout"
if [ -n "${MURMUR_SPEECHD_SOURCE:-}" ]; then
    source=$MURMUR_SPEECHD_SOURCE
    echo "warning: building fotw-speechd from $source, not the pinned FlyOnTheWall revision" >&2
else
    manifest=$(cargo metadata --offline --locked --format-version 1 | python3 -c '
import json, sys
packages = json.load(sys.stdin)["packages"]
print(next(p["manifest_path"] for p in packages if p["name"] == "fotw-speechd-client"))')
    # <checkout>/crates/fotw-speechd-client/Cargo.toml -> <checkout>
    source=$(dirname "$(dirname "$(dirname "$manifest")")")
fi
[ -f "$source/helpers/fotw-speechd/Package.swift" ] ||
    fail "$source has no helpers/fotw-speechd; the pinned FlyOnTheWall revision predates the Swift helper (FlyOnTheWall #253)." \
        "Bump the pin once it is merged, or set MURMUR_SPEECHD_SOURCE for a local-only build."
python3 "$source/tools/speech-eval/parakeet/verify_runtime.py" "$fluid" >&2
target="${CARGO_TARGET_DIR:-$PWD/target}/speechd-swift"
FOTW_FLUIDAUDIO_SOURCE="$fluid" swift build --package-path "$source/helpers/fotw-speechd" \
    --scratch-path "$target" -c release -j "${MURMUR_SWIFT_JOBS:-2}" --product fotw-speechd >&2
printf '%s\n' "$target/release/fotw-speechd"
