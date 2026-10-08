#!/bin/sh
# Build koubou.app: Rust core (static) + libraw (bundled dylib) + SwiftUI shell.
set -e
cd "$(dirname "$0")/.."

LIBRAW_PREFIX="${LIBRAW_PREFIX:-/opt/homebrew}"
APP=mac/build/koubou.app

cargo build --release -p koubou-composer

# araware-core arrives as a git dependency; its staticlib is hashed under deps/
ARAWARE_A=$(ls target/release/deps/libaraware_core-*.a | head -1)

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Frameworks" "$APP/Contents/Resources"
cp mac/Info.plist "$APP/Contents/Info.plist"

# build AppIcon.icns from the committed 1024px source
if [ -f mac/AppIcon.png ]; then
  ICONSET=$(mktemp -d)/AppIcon.iconset
  mkdir -p "$ICONSET"
  for spec in "16 icon_16x16" "32 icon_16x16@2x" "32 icon_32x32" "64 icon_32x32@2x" \
              "128 icon_128x128" "256 icon_128x128@2x" "256 icon_256x256" "512 icon_256x256@2x" \
              "512 icon_512x512" "1024 icon_512x512@2x"; do
    set -- $spec
    sips -z "$1" "$1" mac/AppIcon.png --out "$ICONSET/$2.png" >/dev/null
  done
  iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"
fi

# bundle libraw so the app is self-contained (LGPL dynamic linking preserved)
LIBRAW_DYLIB=$(ls "$LIBRAW_PREFIX"/lib/libraw.*.dylib | head -1)
cp "$LIBRAW_DYLIB" "$APP/Contents/Frameworks/"
DYLIB_BASE=$(basename "$LIBRAW_DYLIB")
install_name_tool -id "@rpath/$DYLIB_BASE" "$APP/Contents/Frameworks/$DYLIB_BASE"

swiftc -O \
  -import-objc-header mac/koubou.h \
  -o "$APP/Contents/MacOS/koubou" \
  mac/Sources/*.swift \
  -L target/release "$ARAWARE_A" -lkoubou_composer \
  -L "$LIBRAW_PREFIX/lib" -lraw \
  -lc++ \
  -framework Foundation -framework AppKit -framework SwiftUI \
  -framework CoreGraphics -framework ImageIO -framework UniformTypeIdentifiers \
  -Xlinker -rpath -Xlinker @executable_path/../Frameworks \
  -Xlinker -rpath -Xlinker "$LIBRAW_PREFIX/lib"

# point the libraw reference inside the binary at the bundled copy
install_name_tool -change "$LIBRAW_DYLIB" "@rpath/$DYLIB_BASE" \
  "$APP/Contents/MacOS/koubou" 2>/dev/null || true

codesign --force --deep --sign - "$APP" 2>/dev/null || true

echo "built: $APP"
