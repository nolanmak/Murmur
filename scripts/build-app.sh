#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
cargo build --locked
TTS_APP="$PWD/Text-to-speech.app"
mkdir -p "$TTS_APP/Contents/MacOS" "$TTS_APP/Contents/Resources"
cp target/debug/text-to-speech "$TTS_APP/Contents/MacOS/text-to-speech"
cp packaging/Info.plist "$TTS_APP/Contents/Info.plist"
cp README.md "$TTS_APP/Contents/Resources/README.md"
TTS_IDENTITY="${TTS_SIGN_IDENTITY:--}"
codesign --force --sign "$TTS_IDENTITY" --identifier com.shipsystems.texttospeech "$TTS_APP"
codesign --verify --deep --strict "$TTS_APP"
printf '%s\n' "Built $TTS_APP (developer signature: $TTS_IDENTITY)"
