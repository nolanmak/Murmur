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

# Nested code before the bundle that seals it. The speech helper (#101) gets
# its own identifier, the hardened runtime and no entitlements: it never opens
# the microphone, Murmur sends it PCM.
helper="$stage/Contents/Helpers/fotw-speechd"
[ "${MURMUR_SPEECHD:-}" != 1 ] || [ -f "$helper" ] ||
    fail "MURMUR_SPEECHD=1 but the bundle lacks Contents/Helpers/fotw-speechd"
if [ -f "$helper" ]; then
    entitlements="$(cd "$(dirname "$0")/.." && pwd)/packaging/speechd-entitlements.plist"
    codesign --force --options runtime --sign "$identity" --identifier "$bundle_id.speechd" \
        --entitlements "$entitlements" "$helper" >/dev/null 2>&1 ||
        fail "codesign failed for the speech helper with \"$identity\""
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
if [ -f "$helper" ]; then
    case $(codesign -d --entitlements :- "$helper" 2>/dev/null || true) in
    *com.apple.security.device.audio-input*) fail "the speech helper carries the microphone entitlement" ;;
    esac
    case $(codesign -d --verbose=2 "$helper" 2>&1 || true) in
    *"(runtime)"*) ;;
    *) fail "the speech helper is not signed with the hardened runtime" ;;
    esac
fi
requirement=$(codesign -d -r- "$stage" 2>&1)
if [ "$identity" != - ]; then
    case $requirement in
    *"designated => cdhash"*)
        fail "\"$identity\" produced a cdhash-only requirement; permissions would reset on rebuild" ;;
    esac
fi

# Own a unique backup directory; never remove a pre-existing user path.
backup=""
if [ -e "$dest" ]; then
    backup=$(mktemp -d "${dest}.previous.XXXXXX") || fail "cannot create bundle backup"
    if ! mv "$dest" "$backup/bundle"; then
        rmdir "$backup"
        fail "cannot stage previous bundle for replacement"
    fi
fi
if ! mv "$stage" "$dest"; then
    if [ -n "$backup" ]; then
        if ! mv "$backup/bundle" "$dest"; then
            rm -rf "$stage"
            printf 'error: install and restore failed; previous bundle retained at %s/bundle\n' "$backup" >&2
            exit 1
        fi
        rmdir "$backup"
    fi
    fail "cannot install verified bundle"
fi
[ -z "$backup" ] || rm -rf "$backup"
printf '%s\n' "Built $dest (developer signature: $identity)"
printf '%s\n' "$requirement" | grep 'designated =>' || true
