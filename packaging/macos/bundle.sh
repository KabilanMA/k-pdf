#!/bin/sh
# Builds dist/k-pdf-<version>-macos-universal.dmg: k-pdf.app (one program for
# Apple Silicon and Intel Macs, with k-pdf-cli inside), signed ad hoc.
# Runs on macOS (Xcode command line tools and both Rust targets installed:
# rustup target add aarch64-apple-darwin x86_64-apple-darwin).
set -eu
cd "$(dirname "$0")/../.."
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' viewer/Cargo.toml | head -1)"
export MACOSX_DEPLOYMENT_TARGET=11.0

for target in aarch64-apple-darwin x86_64-apple-darwin; do
    cargo build --release --workspace --locked --target "$target"
done

APP=dist/k-pdf.app
rm -rf dist/k-pdf.app dist/dmg && mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
for bin in k-pdf k-pdf-cli; do
    lipo -create -output "$APP/Contents/MacOS/$bin" \
        "target/aarch64-apple-darwin/release/$bin" "target/x86_64-apple-darwin/release/$bin"
done
cp packaging/macos/k-pdf.icns "$APP/Contents/Resources/k-pdf.icns"
sed "s/@VERSION@/$VERSION/g" packaging/macos/Info.plist > "$APP/Contents/Info.plist"
# Apple Silicon only runs signed programs: sign ad hoc (no developer ID).
codesign --force --deep --sign - "$APP"

mkdir -p dist/dmg && cp -R "$APP" dist/dmg/ && ln -s /Applications dist/dmg/Applications
cp README.md LICENSE-MIT LICENSE-APACHE dist/dmg/
DMG="dist/k-pdf-$VERSION-macos-universal.dmg"
rm -f "$DMG"
hdiutil create -volname "k-pdf $VERSION" -srcfolder dist/dmg -ov -format UDZO "$DMG" >/dev/null
echo "$DMG"
