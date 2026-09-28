#!/usr/bin/env python3
"""校验 tauri.conf.json 里 ``bundle.icon`` 引用的图标文件。

存在的理由：下面两个约束**只在 Unix 目标上生效**，在 Windows 上完全看不出来，
所以本地开发和 Windows CI 都发现不了，只有 Linux 构建会在
``tauri::generate_context!`` 里 panic（表现为 "proc macro panicked: failed to
open icon ...: No such file or directory"）：

1. ``icons/icon.png`` 必须存在。``tauri-codegen`` 在非 Windows 目标上从
   ``bundle.icon`` 里挑第一个 ``.png``，挑不到就退回硬编码的 ``icons/icon.png``。
2. 那张 PNG 必须是 **RGBA**（色彩类型 6）。``CachedIcon::new_png`` 会检查
   ``png::ColorType::Rgba``，RGB 或调色板都会 panic。

Windows 走的是另一条路（``default_window_icon_from_app_icon_resource``，用
``.ico`` 编出来的资源），所以只跑 Windows 是验证不到这两条的。

CI 的 lint job 会跑这个脚本，把「编译 5 分钟后才炸」变成「1 秒报错」。

用法::

    python tools/check_icons.py
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

CONFIG = Path("crates/cefscan-desktop/src-tauri/tauri.conf.json")

# PNG 色彩类型：6 = 真彩 + alpha。
RGBA = 6

# tauri-codegen 的 find_icon 在挑不到 .png 时用的兜底路径，这里必须跟着变。
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

    # Unix：tauri-codegen 找第一个 .png，语义与 find_icon 保持一致。
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
