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
cp LICENSE "$TTS_STAGE/Contents/Resources/LICENSE"
cp NOTICE "$TTS_STAGE/Contents/Resources/NOTICE"
cp THIRD_PARTY_NOTICES.md "$TTS_STAGE/Contents/Resources/THIRD_PARTY_NOTICES.md"
# Opt-in: MURMUR_SPEECHD=1 bundles the fotw-speechd helper that runs Local
# Parakeet (#101), built from the pinned FlyOnTheWall source by
# scripts/build-speechd.sh. Without it no Swift is needed and Local Parakeet
# reports that this build lacks the helper.
if [ "${MURMUR_SPEECHD:-}" = 1 ]; then
    helper=$(./scripts/build-speechd.sh)
    mkdir -p "$TTS_STAGE/Contents/Helpers"
    cp "$helper" "$TTS_STAGE/Contents/Helpers/fotw-speechd"
fi
exec ./scripts/sign-app.sh "$TTS_STAGE" "$TTS_APP"
