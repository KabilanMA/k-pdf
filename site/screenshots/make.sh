#!/bin/bash
# Makes the website's screenshots (site/static/images/*.webp).
#
#   site/screenshots/make.sh [dark|light|both] [scene ...]
#
# It builds the viewer with the `script` feature, makes the sample files in
# target/site-shots/docs (an article and a form from the LaTeX sources here,
# a synthetic phone photo of a page, a password-protected copy), then runs
# scenes.py. The sample files are made up, so nothing personal ends up on
# the website. Needs pdflatex, ImageMagick 7 (magick), poppler (pdftoppm)
# and mutool. The handwriting scene uses playground/handwritten/
# notes-two-pages.pdf, which is not in git; without it that scene is skipped.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
docs="$root/target/site-shots/docs"
mkdir -p "$docs"

cargo build --release -p k-pdf-viewer --features script --target-dir "$root/target/script" --manifest-path "$root/Cargo.toml"

cd "$docs"
for doc in streams signup; do
    cp "$here/$doc.tex" .
    # Twice, for the table of contents and the outline.
    pdflatex -interaction=nonstopmode "$doc.tex" >/dev/null
    pdflatex -interaction=nonstopmode "$doc.tex" >/dev/null
done
mutool clean -E aes-256 -U secret -O secret streams.pdf locked.pdf

# A phone photo of page 2 lying on a wooden desk: shaded, tilted, in perspective.
pdftoppm -r 110 -png -f 2 -l 2 -singlefile streams.pdf page2
magick page2.png -resize 910x1287! \( -size 910x1287 gradient:'gray(100%)-gray(78%)' \) -compose multiply -composite page2s.png
magick -size 1500x2000 xc:gray50 -seed 7 +noise Gaussian -motion-blur 0x80+90 -colorspace gray -level 30%,70% \
    -colorspace sRGB +level-colors '#4a2c17,#b07d4c' -blur 0x1 -type TrueColor desk.png
magick desk.png -colorspace sRGB \
    \( page2s.png -alpha set -virtual-pixel transparent -define distort:viewport=1500x2000+0+0 \
       -distort Perspective '0,0 260,330  910,0 1270,250  910,1287 1340,1720  0,1287 170,1640' \
       \( +clone -background black -shadow 60x18+14+20 \) +swap -background none -layers merge +repage \) \
    -compose over -composite \
    \( -size 1500x2000 radial-gradient:'gray(100%)-gray(60%)' -colorspace sRGB \) -compose multiply -composite \
    -crop 1500x2000+0+0 +repage -type TrueColor -quality 88 photo.jpg

notes="$root/playground/handwritten/notes-two-pages.pdf"
if [ -f "$notes" ]; then cp "$notes" notes.pdf; fi

python3 "$here/scenes.py" "$@"
