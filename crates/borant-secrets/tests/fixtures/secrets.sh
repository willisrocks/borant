#!/bin/sh
# A fixture secrets plugin of the exec protocol. It answers `describe` and
# `resolve` from a fixed map, so the store test needs no outside backend.
#
# BORANT_FIXTURE_RESOLVE=false: describe a plugin without resolve.
# BORANT_EXPECT_CREDENTIAL=<name>: fail unless the runner set both credential
# variables to <name> and a folder.
set -eu

request=$(cat)

if [ -n "${BORANT_EXPECT_CREDENTIAL:-}" ]; then
    if [ "${BORANT_CREDENTIAL:-}" != "$BORANT_EXPECT_CREDENTIAL" ] ||
        [ ! -d "${BORANT_CREDENTIALS_DIRECTORY:-}" ]; then
        echo '{"error": {"code": "denied", "message": "the credential is not set"}}'
        exit 0
    fi
fi

case "$request" in
*'"op":"describe"'*)
    if [ "${BORANT_FIXTURE_RESOLVE:-true}" = "false" ]; then
        echo '{"ok": {"v": 1, "kind": "secrets", "resolve": false}}'
    else
        echo '{"ok": {"v": 1, "kind": "secrets", "resolve": true, "backend": "fixture"}}'
    fi
    ;;
*'"op":"resolve"'*)
    case "$request" in
    *plugin-error*)
        # The message must never reach the caller's error.
        echo '{"error": {"code": "denied", "message": "s3cret-plugin-error"}}'
        ;;
    *plugin-missing*)
        echo '{"error": {"code": "not_found", "message": "no such ref"}}'
        ;;
    *)
        # Unknown fields of a reply, such as ttl_secs, are ignored.
        echo '{"ok": {"values": {"fixture-token": "fixture-value-one", "fixture-other": "fixture-value-two"}, "backend": "fixture", "ttl_secs": 60}}'
        ;;
    esac
    ;;
*)
    echo '{"error": {"code": "invalid", "message": "unknown operation"}}'
    ;;
esac
