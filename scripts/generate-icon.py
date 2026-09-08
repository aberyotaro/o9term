#!/usr/bin/env python3
"""Draw o9term's Platinum icon and build an ICNS with macOS iconutil.

Uses only Python's standard library. Each size is rendered directly for crisp
pixel edges, rather than shrinking a large bitmap. Edit the 128-unit artwork
in draw_icon to change the design; no fonts or external images are needed.
"""

from pathlib import Path
import struct
import subprocess
import tempfile
import zlib


ROOT = Path(__file__).resolve().parent.parent


class Canvas:
    def __init__(self, size):
        self.size = size
        self.pixels = bytearray(size * size * 4)

    def position(self, value):
        return round(value * self.size / 128)

    def rect(self, left, top, right, bottom, color):
        left, top, right, bottom = map(self.position, (left, top, right, bottom))
        self.pixel_rect(left, top, right, bottom, color)

    def pixel_rect(self, left, top, right, bottom, color):
        left, right = max(0, left), min(self.size, right)
        top, bottom = max(0, top), min(self.size, bottom)
        if left >= right or top >= bottom:
            return
        rgba = bytes((*color, 255)) if len(color) == 3 else bytes(color)
        row = rgba * (right - left)
        for y in range(top, bottom):
            start = (y * self.size + left) * 4
            self.pixels[start:start + len(row)] = row

    def polygon(self, points, color):
        points = [(self.position(x), self.position(y)) for x, y in points]
        for y in range(max(0, min(p[1] for p in points)), min(self.size, max(p[1] for p in points))):
            scan = y + 0.5
            intersections = []
            for (x1, y1), (x2, y2) in zip(points, points[1:] + points[:1]):
                if min(y1, y2) <= scan < max(y1, y2):
                    intersections.append(x1 + (scan - y1) * (x2 - x1) / (y2 - y1))
            intersections.sort()
            for left, right in zip(intersections[::2], intersections[1::2]):
                self.pixel_rect(round(left), y, round(right), y + 1, color)

    def png(self):
        def chunk(kind, data):
            return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))

        stride = self.size * 4
        rows = b''.join(b'\0' + self.pixels[y * stride:(y + 1) * stride] for y in range(self.size))
        header = struct.pack('>IIBBBBB', self.size, self.size, 8, 6, 0, 0, 0)
        return b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', header) + chunk(b'IDAT', zlib.compress(rows, 9)) + chunk(b'IEND', b'')


def draw_icon(size):
    canvas = Canvas(size)
    ink = (37, 37, 37)
    white = (255, 255, 255)
    frame = (204, 204, 204)
    dark = (115, 115, 115)
    light = (238, 238, 238)

    # A restrained, stepped shadow and the square Platinum window frame.
    canvas.rect(18, 23, 117, 113, (0, 0, 0, 45))
    canvas.rect(16, 21, 116, 112, (0, 0, 0, 65))
    canvas.rect(12, 16, 114, 110, ink)
    canvas.rect(13, 17, 113, 109, frame)
    canvas.rect(13, 17, 112, 19, white)
    canvas.rect(13, 19, 15, 108, white)
    canvas.rect(112, 19, 113, 109, dark)
    canvas.rect(15, 107, 112, 109, dark)

    if size >= 32:
        # Ribbed title bar and inset close/zoom controls; simplify the 16px icon.
        canvas.rect(16, 20, 110, 35, frame)
        for y in (22, 25, 28, 31):
            canvas.rect(32, y, 93, y + 1, dark)
            canvas.rect(32, y + 1, 93, y + 2, light)
        for x in (19, 98):
            canvas.rect(x, 22, x + 9, 32, ink)
            canvas.rect(x + 1, 23, x + 8, 31, dark)
            canvas.rect(x + 2, 24, x + 8, 31, white)
            canvas.rect(x + 2, 24, x + 7, 30, frame)
    else:
        canvas.rect(20, 24, 28, 32, ink)
        canvas.rect(40, 24, 101, 28, dark)

    canvas.rect(15, 35, 112, 36, ink)
    canvas.rect(15, 36, 112, 38, white)

    # White terminal, with a sunken edge and a font-independent >_ prompt.
    canvas.rect(17, 40, 109, 103, dark)
    canvas.rect(18, 41, 109, 103, white)
    canvas.polygon([(31, 53), (51, 68), (31, 83), (26, 77), (39, 68), (26, 59)], ink)
    canvas.rect(57, 78, 88, 84, ink)

    if size >= 64:
        # Classic overlapping-square grow box in the lower right.
        canvas.rect(99, 97, 111, 107, frame)
        canvas.rect(100, 98, 108, 105, dark)
        canvas.rect(101, 99, 107, 104, white)
        canvas.rect(103, 101, 111, 108, ink)
        canvas.rect(104, 102, 110, 107, frame)

    return canvas.png()


def main():
    assets = ROOT / 'assets'
    assets.mkdir(exist_ok=True)
    (assets / 'o9term.png').write_bytes(draw_icon(1024))
    with tempfile.TemporaryDirectory(prefix='o9term-icon-') as temporary:
        iconset = Path(temporary) / 'o9term.iconset'
        iconset.mkdir()
        for logical in (16, 32, 128, 256, 512):
            for scale in (1, 2):
                suffix = '@2x' if scale == 2 else ''
                (iconset / f'icon_{logical}x{logical}{suffix}.png').write_bytes(draw_icon(logical * scale))
        subprocess.run(['iconutil', '--convert', 'icns', '--output', str(assets / 'o9term.icns'), str(iconset)], check=True)
    print('Created assets/o9term.png and assets/o9term.icns')


if __name__ == '__main__':
    main()
