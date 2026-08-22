#!/bin/zsh
set -euo pipefail

NATIVE_ROOT="${0:A:h:h}"
BUILD_ROOT="$NATIVE_ROOT/.build"
DEFAULT_SDK="/Library/Developer/CommandLineTools/SDKs/MacOSX15.4.sdk"
NATIVE_SDKROOT="${VISUAL_FILES_SDKROOT:-$DEFAULT_SDK}"
if [[ ! -d "$NATIVE_SDKROOT" ]]; then
    NATIVE_SDKROOT="$(xcrun --sdk macosx --show-sdk-path)"
fi
mkdir -p "$BUILD_ROOT/clang-module-cache"

env \
    SDKROOT="$NATIVE_SDKROOT" \
    CLANG_MODULE_CACHE_PATH="$BUILD_ROOT/clang-module-cache" \
    swift "$NATIVE_ROOT/scripts/summarize-log.swift" "$@"
