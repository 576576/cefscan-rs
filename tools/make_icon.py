#!/usr/bin/env python3
"""生成 cefscanw 的 Windows 图标（纯标准库，不依赖 Pillow）。

图标语义：一块深色圆角底 + 蓝色放大镜，表示"扫描/查找"。

用法::

    python tools/make_icon.py crates/cefscan-desktop/src-tauri/icons

输出 ``icon.ico``（多尺寸 32bpp BMP 条目），tauri-build 生成 Windows
资源文件时需要它。脚本可重复执行，结果确定。
"""

from __future__ import annotations

import math
import struct
import sys
from pathlib import Path

# 输出尺寸（像素）。16/32 给任务栏与列表，48/64 给桌面，128/256 给大图标视图。
SIZES = (16, 32, 48, 64, 128, 256)

# 超采样倍数：先在高分辨率画再盒式降采样，得到抗锯齿边缘。
SUPERSAMPLE = 4

BACKGROUND_TOP = (35, 40, 56)  # #232838
BACKGROUND_BOTTOM = (22, 25, 35)  # #161923
GLASS = (43, 53, 80)  # 镜片内部
ACCENT = (79, 143, 247)  # #4F8FF7


def _rounded_rect_contains(x: float, y: float, size: float, radius: float) -> bool:
    """点 (x, y) 是否落在尺寸 size、圆角半径 radius 的圆角矩形内。"""
    if radius <= 0.0:
        return True
    nearest_x = min(max(x, radius), size - radius)
    nearest_y = min(max(y, radius), size - radius)
    dx = x - nearest_x
    dy = y - nearest_y
    return dx * dx + dy * dy <= radius * radius


def _segment_distance_sq(
    x: float, y: float, ax: float, ay: float, bx: float, by: float
) -> float:
    """点到线段的距离平方（无 sqrt，避免热点里的开方）。"""
    vx = bx - ax
    vy = by - ay
    length_sq = vx * vx + vy * vy
    if length_sq == 0.0:
        return (x - ax) ** 2 + (y - ay) ** 2
    t = ((x - ax) * vx + (y - ay) * vy) / length_sq
    t = 0.0 if t < 0.0 else (1.0 if t > 1.0 else t)
    px = ax + t * vx
    py = ay + t * vy
    return (x - px) ** 2 + (y - py) ** 2


def render_rgba(size: int) -> bytes:
    """渲染单张 size×size 的 RGBA 位图（自上而下，每像素 4 字节）。"""
    n = size * SUPERSAMPLE
    samples = SUPERSAMPLE * SUPERSAMPLE

    radius = 0.22 * n
    cx = cy = 0.42 * n
    outer = 0.235 * n
    ring = 0.070 * n
    glass_radius = outer - ring

    # 放大镜手柄：从镜圈外缘 45° 方向向外延伸。
    diagonal = math.cos(math.radians(45.0))
    handle_ax = cx + diagonal * (outer - ring * 0.6)
    handle_ay = cy + diagonal * (outer - ring * 0.6)
    handle_bx = 0.775 * n
    handle_by = 0.775 * n
    handle_half = 0.085 * n * 0.5

    outer_sq = outer * outer
    glass_sq = glass_radius * glass_radius
    handle_half_sq = handle_half * handle_half

    # 背景竖直渐变，逐行预计算，避免内层重复插值。
    background = []
    for row in range(n):
        t = row / (n - 1) if n > 1 else 0.0
        background.append(
            (
                round(BACKGROUND_TOP[0] + (BACKGROUND_BOTTOM[0] - BACKGROUND_TOP[0]) * t),
                round(BACKGROUND_TOP[1] + (BACKGROUND_BOTTOM[1] - BACKGROUND_TOP[1]) * t),
                round(BACKGROUND_TOP[2] + (BACKGROUND_BOTTOM[2] - BACKGROUND_TOP[2]) * t),
            )
        )

    output = bytearray(size * size * 4)
    write = 0

    for out_y in range(size):
        for out_x in range(size):
            r_sum = g_sum = b_sum = a_sum = 0

            for sy in range(SUPERSAMPLE):
                y = out_y * SUPERSAMPLE + sy + 0.5
                bg = background[int(y) if y < n else n - 1]
                for sx in range(SUPERSAMPLE):
                    x = out_x * SUPERSAMPLE + sx + 0.5

                    if not _rounded_rect_contains(x, y, n, radius):
                        continue  # 圆角外保持透明

                    color = bg
                    dx = x - cx
                    dy = y - cy
                    distance_sq = dx * dx + dy * dy

                    if distance_sq <= glass_sq:
                        color = GLASS
                    if glass_sq <= distance_sq <= outer_sq:
                        color = ACCENT
                    elif _segment_distance_sq(
                        x, y, handle_ax, handle_ay, handle_bx, handle_by
                    ) <= handle_half_sq:
                        color = ACCENT

                    r_sum += color[0]
                    g_sum += color[1]
                    b_sum += color[2]
                    a_sum += 255

            output[write] = r_sum // samples
            output[write + 1] = g_sum // samples
            output[write + 2] = b_sum // samples
            output[write + 3] = a_sum // samples
            write += 4

    return bytes(output)


def _bmp_payload(size: int, rgba: bytes) -> bytes:
    """把 RGBA 位图转成 ICO 条目用的 BMP 结构（BITMAPINFOHEADER + BGRA + AND 掩码）。"""
    header = struct.pack(
        "<IiiHHIIiiII",
        40,  # biSize
        size,  # biWidth
        size * 2,  # biHeight：XOR 图 + AND 掩码，故为两倍
        1,  # biPlanes
        32,  # biBitCount
        0,  # biCompression = BI_RGB
        size * size * 4,  # biSizeImage
        0,  # biXPelsPerMeter
        0,  # biYPelsPerMeter
        0,  # biClrUsed
        0,  # biClrImportant
    )

    # BMP 是自下而上存储的，同时要把 RGBA 调成 BGRA。
    pixels = bytearray()
    for row in range(size - 1, -1, -1):
        start = row * size * 4
        for column in range(size):
            index = start + column * 4
            pixels += bytes(
                (rgba[index + 2], rgba[index + 1], rgba[index], rgba[index + 3])
            )

    # AND 掩码：1bpp，每行补齐到 4 字节边界。32bpp 下 Windows 优先用 alpha，
    # 这里仍然老实写全，兼容老式读取方。
    mask_stride = ((size + 31) // 32) * 4
    mask = bytearray(mask_stride * size)
    for row in range(size):
        source_row = size - 1 - row
        for column in range(size):
            if rgba[(source_row * size + column) * 4 + 3] == 0:
                mask[row * mask_stride + column // 8] |= 0x80 >> (column % 8)

    return header + bytes(pixels) + bytes(mask)


def build_ico(destination: Path) -> None:
    images = []
    for size in SIZES:
        payload = _bmp_payload(size, render_rgba(size))
        images.append((size, payload))

    directory = struct.pack("<HHH", 0, 1, len(images))
    entries = bytearray()
    offset = len(directory) + 16 * len(images)
    for size, payload in images:
        entries += struct.pack(
            "<BBBBHHII",
            0 if size >= 256 else size,  # 0 表示 256
            0 if size >= 256 else size,
            0,  # 调色板颜色数（真彩为 0）
            0,  # 保留
            1,  # 颜色平面
            32,  # 位深
            len(payload),
            offset,
        )
        offset += len(payload)

    destination.parent.mkdir(parents=True, exist_ok=True)
    with destination.open("wb") as handle:
        handle.write(directory)
        handle.write(entries)
        for _, payload in images:
            handle.write(payload)

    print(f"wrote {destination} ({destination.stat().st_size} bytes, {len(images)} sizes)")


def main(argv: list[str]) -> int:
    target = Path(argv[1]) if len(argv) > 1 else Path("icons")
    build_ico(target / "icon.ico")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
