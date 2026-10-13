#!/bin/sh
# Sign a staged Murmur.app, verify it, then replace the installed bundle.
# The installed bundle is only touched after signing and verification succeed.
#
# Identity: MURMUR_SIGN_IDENTITY, TTS_SIGN_IDENTITY, the local "FlyOnTheWall Dev"
# identity when present, otherwise ad-hoc. Ad-hoc grants are pinned to the build's
# cdhash, so macOS drops Accessibility/Input Monitoring on every rebuild.
set -eu
[ $# -eq 2 ] || { echo "usage: $0 STAGED_APP INSTALLED_APP" >&2; exit 2; }
stage=$1
dest=$2
bundle_id=com.shipsystems.texttospeech
dev_identity="FlyOnTheWall Dev"
# FlyOnTheWall's local-only dev keychain; its placeholder password is intentionally public.
keychain="${MURMUR_SIGN_KEYCHAIN:-$HOME/.fotw-dev-cert/fotw-dev.keychain-db}"

fail() {
    rm -rf "$stage"
    printf 'error: %s\n' "$@" >&2
    printf '%s\n' "Previous $dest left unchanged." >&2
    exit 1
}

identity="${MURMUR_SIGN_IDENTITY:-${TTS_SIGN_IDENTITY:-}}"
if [ -z "$identity" ] &&
    security find-identity -v -p codesigning 2>/dev/null | grep -F "\"$dev_identity\"" >/dev/null; then
    identity=$dev_identity
    if [ -f "$keychain" ] &&
        ! security unlock-keychain -p "${MURMUR_SIGN_KEYCHAIN_PASSWORD:-fotw}" "$keychain"; then
        echo "warning: could not unlock $keychain; signing may fail" >&2
    fi
fi
if [ -z "$identity" ]; then
    [ "${MURMUR_REQUIRE_IDENTITY:-}" = 1 ] &&
        fail "no code-signing identity found and MURMUR_REQUIRE_IDENTITY=1"
    cat >&2 <<'EOF'
warning: no code-signing identity found; signing ad-hoc.
warning: macOS ties ad-hoc permissions to this exact build, so Accessibility and
warning: Input Monitoring permissions reset on every rebuild even though System
warning: Settings still shows them enabled. Set MURMUR_SIGN_IDENTITY to a
warning: code-signing certificate to keep permissions across rebuilds.
EOF
    identity=-
fi

if ! err=$(codesign --force --sign "$identity" --identifier "$bundle_id" "$stage" 2>&1); then
    case $err in
    *errSecInternalComponent*)
        fail "codesign could not use \"$identity\": $err" \
            "The keychain holding its private key is probably locked; unlock it with" \
            "  security unlock-keychain <keychain>" ;;
    *) fail "codesign failed for \"$identity\": $err" ;;
    esac
fi
codesign --verify --deep --strict "$stage" || fail "signature verification failed"
requirement=$(codesign -d -r- "$stage" 2>&1)
if [ "$identity" != - ]; then
    case $requirement in
    *"designated => cdhash"*)
        fail "\"$identity\" produced a cdhash-only requirement; permissions would reset on rebuild" ;;
    esac
fi

if [ -e "$dest" ]; then
    rm -rf "$dest.previous"
    mv "$dest" "$dest.previous"
fi
mv "$stage" "$dest"
rm -rf "$dest.previous"
printf '%s\n' "Built $dest (developer signature: $identity)"
printf '%s\n' "$requirement" | grep 'designated =>' || true
