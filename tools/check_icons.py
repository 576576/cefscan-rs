#!/usr/bin/env python3
"""校验 tauri.conf.json 里 ``bundle.icon`` 引用的图标文件。"""

from __future__ import annotations

import json
import sys
from pathlib import Path

CONFIG = Path("crates/cefscan-desktop/src-tauri/tauri.conf.json")

# PNG 色彩类型：6 = 真彩 + alpha。
RGBA = 6

# 挑不到 .png 时使用的兜底路径。
PNG_FALLBACK = "icons/icon.png"


def png_color_type(path: Path) -> int:
    """读 PNG 的 IHDR 分块，返回色彩类型。

    IHDR 固定在最前面：8 字节签名 + 4 字节长度 + 4 字节 ``IHDR``
    + 4 字节宽 + 4 字节高 + 1 字节位深 + 1 字节色彩类型（偏移 25）。
    """
    header = path.read_bytes()[:26]
    if header[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError(f"{path} 的签名不对，不是 PNG")
    if header[12:16] != b"IHDR":
        raise ValueError(f"{path} 的第一个分块不是 IHDR")
    return header[25]


def main() -> int:
    config = json.loads(CONFIG.read_text(encoding="utf-8"))
    icons = config["bundle"]["icon"]
    root = CONFIG.parent

    problems: list[str] = []

    # Windows：tauri-build 找第一个 .ico 去生成资源文件。
    ico = next((name for name in icons if name.endswith(".ico")), "icons/icon.ico")
    if not (root / ico).is_file():
        problems.append(f"Windows 资源图标缺失：{ico}")

    # Unix：tauri-codegen 找第一个 .png。
    png = next((name for name in icons if name.endswith(".png")), PNG_FALLBACK)
    png_path = root / png
    if not png_path.is_file():
        problems.append(f"Unix 目标窗口图标缺失：{png}（bundle.icon 里没有 .png 时就会退回这个路径）")
    else:
        color_type = png_color_type(png_path)
        if color_type != RGBA:
            problems.append(f"{png} 的色彩类型是 {color_type}，tauri 要求 RGBA({RGBA})")

    if problems:
        for problem in problems:
            print(f"error: {problem}", file=sys.stderr)
        return 1

    print(f"图标齐全：{ico}（Windows）、{png}（Unix）")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
