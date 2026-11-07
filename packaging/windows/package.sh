#!/bin/sh
# Builds dist/k-pdf-<version>-windows-x86_64.zip: k-pdf.exe, k-pdf-cli.exe,
# README and licenses. Runs on Windows in Git Bash (as on GitHub Actions).
set -eu
cd "$(dirname "$0")/../.."
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' viewer/Cargo.toml | head -1)"
NAME="k-pdf-$VERSION-windows-x86_64"
cargo build --release --workspace --locked
rm -rf "dist/$NAME" && mkdir -p "dist/$NAME"
cp target/release/k-pdf.exe target/release/k-pdf-cli.exe README.md LICENSE-MIT LICENSE-APACHE "dist/$NAME/"
(cd dist && 7z a -tzip "$NAME.zip" "$NAME" >/dev/null)
echo "dist/$NAME.zip"
