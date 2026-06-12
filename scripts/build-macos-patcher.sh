#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" || "$(uname -m)" != "arm64" ]]; then
  echo "The macOS patcher can only be built on Apple Silicon." >&2
  exit 1
fi

profile="${1:-debug}"
case "$profile" in
  debug)
    target_profile="debug"
    cargo build -p ltk-macos-patcher --target "aarch64-apple-darwin"
    ;;
  release)
    target_profile="release"
    cargo build -p ltk-macos-patcher --target "aarch64-apple-darwin" --release
    ;;
  *)
    echo "Usage: $0 [debug|release]" >&2
    exit 1
    ;;
esac

target="aarch64-apple-darwin"

source_path="target/$target/$target_profile/ltk-macos-patcher"
destination="src-tauri/binaries/ltk-macos-patcher-$target"
mkdir -p "$(dirname "$destination")"
install -m 0755 "$source_path" "$destination"

# An ad-hoc signature keeps local app and helper builds deterministic without
# introducing Developer ID or notarization requirements.
/usr/bin/codesign --force --sign - --timestamp=none "$destination"

echo "Built $destination"
