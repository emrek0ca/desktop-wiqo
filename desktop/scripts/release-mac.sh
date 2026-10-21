#!/bin/bash
# Builds a Mac release of Wiqo: signed with Developer ID, notarized and stapled by Apple, plus the signed update archive and latest.json the
# updater reads from https://api.wiqoai.com/desktop-updates. Nothing is uploaded; the last lines say what to copy where.
#
# One-time setup:
#   xcrun notarytool store-credentials wiqo-notary --apple-id <apple id> --team-id 7SRKKF75MC   (asks for an app-specific password)
#   The updater key is ~/.tauri/wiqo-updater.key (its public half is in app/tauri.conf.json); keep a copy somewhere safe: without it no
#   installed Wiqo accepts another update.
set -euo pipefail

cd "$(dirname "$0")/.."
KEY="${WIQO_UPDATER_KEY:-$HOME/.tauri/wiqo-updater.key}"
PROFILE="${WIQO_NOTARY_PROFILE:-wiqo-notary}"
VERSION=$(python3 -c 'import json; print(json.load(open("app/tauri.conf.json"))["version"])')
ARCH=$(uname -m | sed 's/arm64/aarch64/')
OUT="release/$VERSION"
APP="target/release/bundle/macos/Wiqo.app"
NOTES="${1:-}"

[ -f "$KEY" ] || { echo "updater key missing: $KEY"; exit 1; }
xcrun notarytool history --keychain-profile "$PROFILE" >/dev/null 2>&1 || { echo "notary profile '$PROFILE' missing (see the setup lines at the top)"; exit 1; }

echo "== build $VERSION ($ARCH)"
(cd app && cargo tauri build --bundles app)

echo "== notarize the app"
mkdir -p "$OUT"
ditto -c -k --keepParent "$APP" "$OUT/notarize.zip"
xcrun notarytool submit "$OUT/notarize.zip" --keychain-profile "$PROFILE" --wait
xcrun stapler staple "$APP"
rm "$OUT/notarize.zip"

echo "== update archive"
ARCHIVE="Wiqo_${VERSION}_${ARCH}.app.tar.gz"
tar -czf "$OUT/$ARCHIVE" -C "$(dirname "$APP")" Wiqo.app
TAURI_SIGNING_PRIVATE_KEY_PASSWORD="" cargo tauri signer sign -f "$KEY" -p "" "$OUT/$ARCHIVE" >/dev/null
SIGNATURE=$(cat "$OUT/$ARCHIVE.sig")

echo "== installer disk image"
DMG="Wiqo_${VERSION}_${ARCH}.dmg"
hdiutil create -volname Wiqo -srcfolder "$APP" -ov -format UDZO "$OUT/$DMG" >/dev/null
codesign --sign "Developer ID Application: Osman Emre Koca (7SRKKF75MC)" "$OUT/$DMG"
xcrun notarytool submit "$OUT/$DMG" --keychain-profile "$PROFILE" --wait
xcrun stapler staple "$OUT/$DMG"

python3 - "$OUT" "$VERSION" "$ARCH" "$ARCHIVE" "$SIGNATURE" "$NOTES" <<'PY'
import json, sys, datetime, os
out, version, arch, archive, signature, notes = sys.argv[1:]
path = os.path.join(out, "latest.json")
release = {"version": version, "notes": notes, "pub_date": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
           "platforms": {f"darwin-{arch}": {"signature": signature, "url": f"https://api.wiqoai.com/desktop-updates/files/{archive}"}}}
json.dump(release, open(path, "w"), indent=2)
PY

echo
echo "Ready in $OUT:"
ls -1 "$OUT"
echo
echo "To publish (after you say go): copy $ARCHIVE and latest.json to /var/www/rova-backend/desktop-updates/ on the server;"
echo "the backend must be deployed with routes/desktopUpdates.ts. The .dmg is the first-install download."
