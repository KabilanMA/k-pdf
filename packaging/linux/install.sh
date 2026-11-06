#!/bin/sh
# Installs (or removes) k-pdf for the current user, or system-wide.
#
#   ./install.sh                 install into ~/.local
#   sudo PREFIX=/usr/local ./install.sh
#   ./install.sh --default       also make k-pdf the default PDF application
#   ./install.sh --uninstall     remove what was installed
#
# Run it from an unpacked release (the programs are next to it) or from the
# source tree after `cargo build --release --workspace`.
set -eu

PREFIX="${PREFIX:-$HOME/.local}"
HERE="$(cd "$(dirname "$0")" && pwd)"
BIN="$PREFIX/bin"
APPS="$PREFIX/share/applications"
ICONS="$PREFIX/share/icons/hicolor"

refresh() {
    command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q "$APPS" || true
    command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q -t "$ICONS" || true
}

case "${1:-}" in
--uninstall)
    rm -f "$BIN/k-pdf" "$BIN/k-pdf-cli" "$APPS/k-pdf.desktop" "$ICONS/scalable/apps/k-pdf.svg"
    refresh
    echo "k-pdf removed from $PREFIX"
    exit 0
    ;;
""|--default) ;;
*)
    sed -n '2,10p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
    ;;
esac

# The programs: next to this script (release), or in the build output.
if [ -x "$HERE/k-pdf" ]; then
    FROM="$HERE"
elif [ -x "$HERE/../../target/release/k-pdf" ]; then
    FROM="$HERE/../../target/release"
else
    echo "k-pdf isn't built yet: run 'cargo build --release --workspace' first" >&2
    exit 1
fi

mkdir -p "$BIN" "$APPS" "$ICONS/scalable/apps"
install -m 755 "$FROM/k-pdf" "$BIN/k-pdf"
[ -x "$FROM/k-pdf-cli" ] && install -m 755 "$FROM/k-pdf-cli" "$BIN/k-pdf-cli"
install -m 644 "$HERE/k-pdf.svg" "$ICONS/scalable/apps/k-pdf.svg"
# Launchers don't always search ~/.local/bin: point at the program itself.
sed "s|^Exec=k-pdf |Exec=$BIN/k-pdf |" "$HERE/k-pdf.desktop" > "$APPS/k-pdf.desktop"
chmod 644 "$APPS/k-pdf.desktop"
refresh

if [ "${1:-}" = "--default" ] && command -v xdg-mime >/dev/null 2>&1; then
    xdg-mime default k-pdf.desktop application/pdf
    echo "k-pdf is now the default application for PDF files"
fi
echo "k-pdf installed in $PREFIX (find it in your applications menu; k-pdf-cli is the command-line tool)"
