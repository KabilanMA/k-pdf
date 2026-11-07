#!/usr/bin/env python3
"""Makes every program icon from packaging/linux/k-pdf.svg (needs rsvg-convert
and ImageMagick's `magick`). Run from the repository root after changing the
SVG:

    python3 packaging/make-icons.py

Writes packaging/windows/k-pdf.ico, packaging/macos/k-pdf.icns and
viewer/assets/icon-128.rgba (the window icon).
"""
import struct, subprocess, sys

svg, ico_out, icns_out, rgba_out = (sys.argv[1:5] if len(sys.argv) == 5 else (
    'packaging/linux/k-pdf.svg', 'packaging/windows/k-pdf.ico',
    'packaging/macos/k-pdf.icns', 'viewer/assets/icon-128.rgba'))
def png(size):
    return subprocess.run(['rsvg-convert', '-w', str(size), '-h', str(size), svg], check=True, capture_output=True).stdout
# Windows .ico: PNG-compressed entries.
sizes = [16, 24, 32, 48, 64, 128, 256]
images = [png(s) for s in sizes]
out = struct.pack('<HHH', 0, 1, len(sizes))
offset = 6 + 16 * len(sizes)
for s, data in zip(sizes, images):
    out += struct.pack('<BBBBHHII', s % 256, s % 256, 0, 0, 1, 32, len(data), offset)
    offset += len(data)
open(ico_out, 'wb').write(out + b''.join(images))
# macOS .icns: PNG entries with a table of contents.
types = [(b'ic11', 32), (b'ic12', 64), (b'ic07', 128), (b'ic13', 256), (b'ic08', 256), (b'ic14', 512), (b'ic09', 512), (b'ic10', 1024)]
entries = [(t, png(s)) for t, s in types]
toc = b''.join(t + struct.pack('>I', 8 + len(d)) for t, d in entries)
body = b'TOC ' + struct.pack('>I', 8 + len(toc)) + toc + b''.join(t + struct.pack('>I', 8 + len(d)) + d for t, d in entries)
open(icns_out, 'wb').write(b'icns' + struct.pack('>I', 8 + len(body)) + body)
# Window icon: 128 × 128 raw RGBA.
subprocess.run(['magick', 'png:-', '-depth', '8', 'rgba:' + rgba_out], input=png(128), check=True)
