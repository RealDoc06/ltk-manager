#!/usr/bin/env bash
set -euo pipefail

app="target/aarch64-apple-darwin/release/bundle/macos/LTK Manager.app"
helper="$app/Contents/MacOS/ltk-macos-patcher"

if [[ ! -d "$app" || ! -x "$helper" ]]; then
  echo "Build the ARM64 app bundle before signing: pnpm macos:build" >&2
  exit 1
fi

/usr/bin/codesign --force --sign - --timestamp=none "$helper"
/usr/bin/codesign --force --sign - --timestamp=none "$app"
/usr/bin/codesign --verify --deep --strict --verbose=2 "$app"

echo "Signed $app"
