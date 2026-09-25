#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
cargo build --locked
TTS_APP="$PWD/Murmur.app"
# Assemble beside target/ so a failed signature never touches the installed app.
TTS_STAGE="$PWD/target/stage/Murmur.app"
rm -rf "$TTS_STAGE"
mkdir -p "$TTS_STAGE/Contents/MacOS" "$TTS_STAGE/Contents/Resources"
cp target/debug/murmur "$TTS_STAGE/Contents/MacOS/murmur"
cp packaging/Info.plist "$TTS_STAGE/Contents/Info.plist"
cp README.md "$TTS_STAGE/Contents/Resources/README.md"
exec ./scripts/sign-app.sh "$TTS_STAGE" "$TTS_APP"
