#!/bin/zsh
set -euo pipefail

NATIVE_ROOT="${0:A:h:h}"
BUILD_ROOT="$NATIVE_ROOT/.build"
DEFAULT_SDK="/Library/Developer/CommandLineTools/SDKs/MacOSX15.4.sdk"
NATIVE_SDKROOT="${VISUAL_FILES_SDKROOT:-$DEFAULT_SDK}"
if [[ ! -d "$NATIVE_SDKROOT" ]]; then
    NATIVE_SDKROOT="$(xcrun --sdk macosx --show-sdk-path)"
fi

mkdir -p "$BUILD_ROOT/clang-module-cache" "$BUILD_ROOT/swiftpm-module-cache"
mkdir -p "$BUILD_ROOT/cache" "$BUILD_ROOT/config" "$BUILD_ROOT/security"
env \
    SDKROOT="$NATIVE_SDKROOT" \
    CLANG_MODULE_CACHE_PATH="$BUILD_ROOT/clang-module-cache" \
    SWIFTPM_MODULECACHE_OVERRIDE="$BUILD_ROOT/swiftpm-module-cache" \
    swift build \
        --package-path "$NATIVE_ROOT" \
        --configuration release \
        --disable-sandbox \
        --cache-path "$BUILD_ROOT/cache" \
        --config-path "$BUILD_ROOT/config" \
        --security-path "$BUILD_ROOT/security"

"$BUILD_ROOT/arm64-apple-macosx/release/VisualFilesNative" --smoke

"$NATIVE_ROOT/scripts/build-app.sh" >/dev/null
APP_PATH="$NATIVE_ROOT/dist/Visual Files Native.app"
LAUNCH_SMOKE_DIR="$(mktemp -d /private/tmp/visual-files-native-launch-smoke.XXXXXX)"
trap 'rm -rf "$LAUNCH_SMOKE_DIR"' EXIT
open -W -n -g \
    --env "VISUAL_FILES_LOG_DIR=$LAUNCH_SMOKE_DIR" \
    --env VISUAL_FILES_SHORTCUT_KEYCODE=25 \
    --env VISUAL_FILES_SHORTCUT_MODIFIERS=command,shift \
    "$APP_PATH" \
    --args --launch-smoke

if ! /usr/bin/grep -q '"event":"backend_ready".*"shortcut_registered":true' "$LAUNCH_SMOKE_DIR/events.ndjson"; then
    echo "launch smoke did not record successful shortcut registration" >&2
    exit 1
fi
echo "launch smoke passed: backend ready and temporary shortcut registered"
