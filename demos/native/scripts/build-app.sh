#!/bin/zsh
set -euo pipefail

NATIVE_ROOT="${0:A:h:h}"
BUILD_ROOT="$NATIVE_ROOT/.build"
DIST_ROOT="$NATIVE_ROOT/dist"
APP_PATH="$DIST_ROOT/Visual Files Native.app"
CONTENTS_PATH="$APP_PATH/Contents"

# The installed CLT's default 26.5 SDK and swiftc differ by one internal build.
# Its 15.4 SDK is compatible with the same Swift 6.3.2 compiler and our macOS 14 target.
DEFAULT_SDK="/Library/Developer/CommandLineTools/SDKs/MacOSX15.4.sdk"
NATIVE_SDKROOT="${VISUAL_FILES_SDKROOT:-$DEFAULT_SDK}"
if [[ ! -d "$NATIVE_SDKROOT" ]]; then
    NATIVE_SDKROOT="$(xcrun --sdk macosx --show-sdk-path)"
fi

mkdir -p "$BUILD_ROOT/clang-module-cache" "$BUILD_ROOT/swiftpm-module-cache"
mkdir -p "$BUILD_ROOT/cache" "$BUILD_ROOT/config" "$BUILD_ROOT/security" "$DIST_ROOT"

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

rm -rf "$APP_PATH"
mkdir -p "$CONTENTS_PATH/MacOS" "$CONTENTS_PATH/Resources"
cp "$BUILD_ROOT/arm64-apple-macosx/release/VisualFilesNative" "$CONTENTS_PATH/MacOS/VisualFilesNative"
cp "$NATIVE_ROOT/Resources/Info.plist" "$CONTENTS_PATH/Info.plist"
codesign --force --sign - "$APP_PATH"

echo "$APP_PATH"
