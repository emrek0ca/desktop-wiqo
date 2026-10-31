#!/usr/bin/env bash
# Builds the pinned agent-desktop helper (Apache-2.0) that Wiqo uses to drive other apps' controls (computerDesktop).
# Usage: scripts/build-helper.sh <target-triple|universal-apple-darwin> <output-dir>
#   -> <output-dir>/agent-desktop[.exe] and agent-desktop.LICENSE; the bundle puts the program next to the Wiqo executable.
set -euo pipefail
PIN=a47ed664e0db00d1d4df364eb3fe8a5599882f3c   # vercel-labs/agent-desktop v1.0.2
TARGET="${1:?target triple, or universal-apple-darwin}"
OUT="${2:?output directory}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
git clone --quiet https://github.com/vercel-labs/agent-desktop "$WORK/src"
git -C "$WORK/src" checkout --quiet "$PIN"
# macOS 27 lists exited processes with pid -1, which made the whole app inventory "incomplete"; skip them.
git -C "$WORK/src" apply "$HERE/helper/agent-desktop-macos-pid.patch"
mkdir -p "$OUT"
build() { cargo build --release --manifest-path "$WORK/src/Cargo.toml" -p agent-desktop --target "$1"; }
if [ "$TARGET" = "universal-apple-darwin" ]; then
  rustup target add aarch64-apple-darwin x86_64-apple-darwin >/dev/null 2>&1 || true
  build aarch64-apple-darwin
  build x86_64-apple-darwin
  lipo -create -output "$OUT/agent-desktop" \
    "$WORK/src/target/aarch64-apple-darwin/release/agent-desktop" "$WORK/src/target/x86_64-apple-darwin/release/agent-desktop"
else
  build "$TARGET"
  case "$TARGET" in
    *windows*) cp "$WORK/src/target/$TARGET/release/agent-desktop.exe" "$OUT/agent-desktop.exe" ;;
    *) cp "$WORK/src/target/$TARGET/release/agent-desktop" "$OUT/agent-desktop" ;;
  esac
fi
cp "$WORK/src/LICENSE" "$OUT/agent-desktop.LICENSE"
echo "built into $OUT: $(ls "$OUT" | tr '\n' ' ')"
