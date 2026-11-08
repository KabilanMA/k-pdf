#!/bin/sh
# Builds a release archive: dist/k-pdf-<version>-linux-<arch>.tar.gz, with
# k-pdf, k-pdf-cli, the launcher, the icon, install.sh and the
# text-recognition models (models/, for finding words in images).
set -eu
cd "$(dirname "$0")/../.."
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' viewer/Cargo.toml | head -1)"
NAME="k-pdf-$VERSION-linux-$(uname -m)"
cargo build --release --workspace --locked
rm -rf "dist/$NAME" && mkdir -p "dist/$NAME"
cp target/release/k-pdf target/release/k-pdf-cli "dist/$NAME/"
strip "dist/$NAME/k-pdf" "dist/$NAME/k-pdf-cli" 2>/dev/null || true
packaging/fetch-ocr-models.sh "dist/$NAME/models"
cp packaging/linux/k-pdf.desktop packaging/linux/k-pdf.svg packaging/linux/install.sh README.md LICENSE-MIT LICENSE-APACHE "dist/$NAME/"
tar -C dist -czf "dist/$NAME.tar.gz" "$NAME"
echo "dist/$NAME.tar.gz"
