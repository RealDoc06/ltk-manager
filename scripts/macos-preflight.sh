#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "--" ]]; then
  shift
fi

if [[ $# -ne 1 ]]; then
  echo "Usage: pnpm macos:preflight -- '/Applications/League of Legends.app'" >&2
  exit 1
fi

bundle="$1"
executable="$bundle/Contents/LoL/Game/LeagueofLegends.app/Contents/MacOS/LeagueofLegends"
helper="src-tauri/binaries/ltk-macos-patcher-aarch64-apple-darwin"

if [[ ! -x "$helper" ]]; then
  bash scripts/build-macos-patcher.sh debug
fi

"$helper" --preflight "$executable"
