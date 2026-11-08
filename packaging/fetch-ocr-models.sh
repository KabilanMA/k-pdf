#!/bin/sh
# Downloads the text-recognition (OCR) models k-pdf uses to find words in
# images (see src/ocr.rs): text-detection.onnx and text-recognition.onnx,
# from the ocrs project (https://github.com/robertknight/ocrs-models),
# checked against known SHA-256 sums.
#
#   packaging/fetch-ocr-models.sh            # into the user's data folder
#   packaging/fetch-ocr-models.sh DIR        # into DIR (used for packages)
set -eu

BASE="https://ocrs-models.s3-accelerate.amazonaws.com"
DETECTION_SHA="a917b23dbd9524b465df7e922641b3ff2981623df4ded5a0234004ef2fee7cfe"
RECOGNITION_SHA="86c145c2edb96c8caed5b1ebb8f44d706408922211451c309c625157dd6061c5"

if [ $# -ge 1 ]; then
    DIR="$1"
elif [ "$(uname)" = "Darwin" ]; then
    DIR="$HOME/Library/Application Support/k-pdf/models"
else
    DIR="${XDG_DATA_HOME:-$HOME/.local/share}/k-pdf/models"
fi
mkdir -p "$DIR"

sha256() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

fetch() {
    name="$1"; sum="$2"
    if [ -f "$DIR/$name" ] && [ "$(sha256 "$DIR/$name")" = "$sum" ]; then
        echo "$DIR/$name is up to date"
        return
    fi
    curl -fsSL "$BASE/$name" -o "$DIR/$name.part"
    if [ "$(sha256 "$DIR/$name.part")" != "$sum" ]; then
        rm -f "$DIR/$name.part"
        echo "error: $name doesn't match its known checksum" >&2
        exit 1
    fi
    mv "$DIR/$name.part" "$DIR/$name"
    echo "downloaded $DIR/$name"
}

fetch text-detection.onnx "$DETECTION_SHA"
fetch text-recognition.onnx "$RECOGNITION_SHA"
