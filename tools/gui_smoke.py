#!/usr/bin/env python3
"""cefscanw 的端到端冒烟测试（Windows，仅手动运行，不进 `cargo test`）。

它会真的去点 GUI：找窗口 → 定位控件 → 输入限定目录 → 点「开始扫描」→ 定时截屏。
用来验证「Tauri command + Channel + 前端渲染」这条链路确实通，而不仅仅是能编译。

用法::

    cargo build --release
    ./target/release/cefscanw.exe &
    python tools/gui_smoke.py out.png C:/Users/<you> 6 20

后两个参数是截屏时刻（秒），可给多个。脚本不会关掉 cefscanw，自己收尾。

设 `CEFSCAN_SMOKE_EXPAND=1` 可以额外验证「点整行展开路径」：截屏结束后点第一行
数据行，再存一张 `*_expanded.png`。

设计要点：**不写死控件坐标**，而是从像素里认控件，这样改布局也不用改脚本：

* 找窗口：`EnumWindows` + 标题前缀。
* 找「开始扫描」按钮：在窗口矩形内找强调色 `#4f8ff7` 的像素，按 x 聚类后取
  **最右**一簇。最左那簇是窗口自身边框的蓝色，必须排除。
* 找输入框：在按钮同一行上、**按钮左侧**找 `#1a1c21` 最长的一段连续像素。
  两个约束都必要：窗口底色 `#1b1d23` 与输入框底色只差 2，单靠容差分不开，
  所以既要收紧容差（`FIELD_TOLERANCE`），又要把搜索范围截到按钮左边。
* 输入文字：`SendInput` + `KEYEVENTF_UNICODE`，绕开键盘布局。
* 截屏：GDI `BitBlt`，手写 PNG 编码 —— 本机没有 Pillow，`Add-Type` 也被安全策略拦了。

已知脆弱点：依赖窗口完整可见、未被遮挡；颜色/尺寸若大改需要同步更新常量。
"""

import ctypes
import ctypes.wintypes as wintypes
import os
import struct
import sys
import time
import zlib

user32 = ctypes.WinDLL("user32", use_last_error=True)
gdi32 = ctypes.WinDLL("gdi32", use_last_error=True)

SRCCOPY = 0x00CC0020
SW_RESTORE = 9
MOUSEEVENTF_LEFTDOWN = 0x0002
MOUSEEVENTF_LEFTUP = 0x0004
KEYEVENTF_UNICODE = 0x0004
KEYEVENTF_KEYUP = 0x0002
INPUT_KEYBOARD = 1

# 与 ui/styles.css 里的 --accent / #root-input 背景保持一致。
ACCENT = (247, 143, 79)  # BGRA 顺序的 #4f8ff7
FIELD = (33, 28, 26)  # BGRA 顺序的 #1a1c21
TOLERANCE = 14
# 窗口底色 #1b1d23 的 BGRA 是 (35, 29, 27)，跟 FIELD 只差 2，必须收紧才分得开。
FIELD_TOLERANCE = 4

# KIND_COLORS（见 ui/main.js）里各标签底色，BGRA 顺序，用来定位数据行。
#
# **刻意不含 unknown `#7f848e`**：它接近中性灰（BGRA 142,132,127），跟界面上
# 抗锯齿文字的混色像素几乎分不开，收进来会让汇总区那一行也被当成数据行。
TAG_COLORS = [
    (228, 196, 125),  # electron   #7dc4e4
    (230, 169, 90),  # edge       #5aa9e6
    (117, 108, 224),  # chrome     #e06c75
    (221, 120, 198),  # nwjs       #c678dd
    (123, 192, 229),  # cefsharp   #e5c07b
    (121, 195, 152),  # mini_electron #98c379
    (194, 182, 86),  # mini_blink #56b6c2
    (102, 154, 209),  # cef        #d19a66
]


# ---------------------------------------------------------------- 窗口


def find_window(prefix):
    """按标题前缀找顶层窗口，返回 (hwnd, title)。"""
    found = []

    @ctypes.WINFUNCTYPE(ctypes.c_int, wintypes.HWND, wintypes.LPARAM)
    def callback(hwnd, _lparam):
        length = user32.GetWindowTextLengthW(hwnd)
        if length:
            buffer = ctypes.create_unicode_buffer(length + 1)
            user32.GetWindowTextW(hwnd, buffer, length + 1)
            if buffer.value.startswith(prefix):
                found.append((hwnd, buffer.value))
        return 1

    user32.EnumWindows(callback, 0)
    return found[0] if found else (None, None)


def focus(hwnd):
    if user32.IsIconic(hwnd):
        user32.ShowWindow(hwnd, SW_RESTORE)
    user32.SetForegroundWindow(hwnd)
    time.sleep(0.6)


def window_region(hwnd):
    rect = wintypes.RECT()
    if not user32.GetWindowRect(hwnd, ctypes.byref(rect)):
        raise OSError("GetWindowRect failed")
    return (rect.left, rect.top, rect.right, rect.bottom)


# ---------------------------------------------------------------- 截屏


class BitmapInfoHeader(ctypes.Structure):
    _fields_ = [
        ("biSize", wintypes.DWORD),
        ("biWidth", ctypes.c_long),
        ("biHeight", ctypes.c_long),
        ("biPlanes", wintypes.WORD),
        ("biBitCount", wintypes.WORD),
        ("biCompression", wintypes.DWORD),
        ("biSizeImage", wintypes.DWORD),
        ("biXPelsPerMeter", ctypes.c_long),
        ("biYPelsPerMeter", ctypes.c_long),
        ("biClrUsed", wintypes.DWORD),
        ("biClrImportant", wintypes.DWORD),
    ]


def capture():
    """整屏抓图，返回 (width, height, BGRA bytes)。像素自上而下排列。"""
    width = user32.GetSystemMetrics(0)
    height = user32.GetSystemMetrics(1)

    screen_dc = user32.GetDC(0)
    memory_dc = gdi32.CreateCompatibleDC(screen_dc)
    bitmap = gdi32.CreateCompatibleBitmap(screen_dc, width, height)
    gdi32.SelectObject(memory_dc, bitmap)
    if not gdi32.BitBlt(memory_dc, 0, 0, width, height, screen_dc, 0, 0, SRCCOPY):
        raise OSError("BitBlt failed")

    header = BitmapInfoHeader()
    header.biSize = ctypes.sizeof(BitmapInfoHeader)
    header.biWidth = width
    header.biHeight = -height  # 负值 = 自上而下
    header.biPlanes = 1
    header.biBitCount = 32
    header.biCompression = 0

    buffer = ctypes.create_string_buffer(width * height * 4)
    if not gdi32.GetDIBits(memory_dc, bitmap, 0, height, buffer, ctypes.byref(header), 0):
        raise OSError("GetDIBits failed")

    gdi32.DeleteObject(bitmap)
    gdi32.DeleteDC(memory_dc)
    user32.ReleaseDC(0, screen_dc)
    return width, height, buffer.raw


def save_png(path, width, pixels, region):
    """把 BGRA 整屏缓冲的某个矩形区域写成 PNG。"""
    left, top, right, bottom = region
    crop_width = right - left
    crop_height = bottom - top

    rows = []
    for y in range(top, bottom):
        start = y * width * 4
        row = bytearray([0])  # PNG 每行的 filter byte
        for x in range(left, right):
            index = start + x * 4
            row += bytes((pixels[index + 2], pixels[index + 1], pixels[index]))
        rows.append(bytes(row))
    raw = b"".join(rows)

    def chunk(tag, payload):
        body = tag + payload
        return (
            struct.pack(">I", len(payload))
            + body
            + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)
        )

    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", crop_width, crop_height, 8, 2, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(raw, 6))
    png += chunk(b"IEND", b"")
    with open(path, "wb") as handle:
        handle.write(png)


# ---------------------------------------------------------------- 像素定位


def close_to(pixel, target, tolerance=TOLERANCE):
    return (
        abs(pixel[0] - target[0]) <= tolerance
        and abs(pixel[1] - target[1]) <= tolerance
        and abs(pixel[2] - target[2]) <= tolerance
    )


def find_button(pixels, width, region):
    """找强调色块。按 x 方向聚类，取最右一簇（按钮在输入框右侧）。"""
    left, top, right, bottom = region
    columns = {}
    for y in range(top, bottom):
        row = y * width * 4
        for x in range(left, right):
            index = row + x * 4
            if close_to(pixels[index : index + 3], ACCENT):
                columns.setdefault(x, []).append(y)

    if not columns:
        return None

    xs = sorted(columns)
    clusters = [[xs[0]]]
    for x in xs[1:]:
        if x - clusters[-1][-1] <= 6:  # 间隔 > 6px 视为另一簇
            clusters[-1].append(x)
        else:
            clusters.append([x])

    print(f"强调色簇: {[(c[0], c[-1], len(c)) for c in clusters]}")
    chosen = clusters[-1]
    ys = [y for x in chosen for y in columns[x]]
    return (
        (chosen[0] + chosen[-1]) // 2,
        (min(ys) + max(ys)) // 2,
        (chosen[0], min(ys), chosen[-1], max(ys)),
    )


def find_field(pixels, width, y, region, right_limit):
    """在给定行、`right_limit` 左侧找输入框，返回 (点击x, 宽度)。

    用匹配像素的**最小/最大 x** 定边界，而不是找最长连续段：输入框里已经有文字
    （上一轮输进去的路径）时，连续段会被文字切碎，只剩几十像素的碎片。
    返回的 x 落在框内左侧内边距上，点哪儿都是把光标放进框里。
    """
    left, _top, _right, _bottom = region
    hits = []
    for x in range(left, right_limit):
        index = (y * width + x) * 4
        if close_to(pixels[index : index + 3], FIELD, FIELD_TOLERANCE):
            hits.append(x)
    if not hits:
        return None
    start, end = min(hits), max(hits)
    return start + 10, end - start


def find_first_row(pixels, width, region, min_hits=40):
    """找第一条数据行。用「类型」列的标签底色定位——它在表格里独一无二。

    要求一行里至少有 `min_hits` 个匹配像素，这样才不会被应用图标里偶合的
    颜色骗到（图标只有 18px，一个标签的底色有几十像素宽）。
    从工具条下方（+90px）开始扫，避开工具条和汇总区的文字。
    返回标签中心 (x, y)；找不到返回 None。
    """
    left, top, right, bottom = region
    for y in range(top + 90, bottom):
        row = y * width * 4
        hits = []
        for x in range(left, right):
            index = row + x * 4
            for tag in TAG_COLORS:
                if close_to(pixels[index : index + 3], tag):
                    hits.append(x)
                    break
        if len(hits) >= min_hits:
            return (min(hits) + max(hits)) // 2, y
    return None


# ---------------------------------------------------------------- 输入注入


class MouseInput(ctypes.Structure):
    _fields_ = [
        ("dx", ctypes.c_long),
        ("dy", ctypes.c_long),
        ("mouseData", wintypes.DWORD),
        ("dwFlags", wintypes.DWORD),
        ("time", wintypes.DWORD),
        ("dwExtraInfo", ctypes.POINTER(ctypes.c_ulong)),
    ]


class KeybdInput(ctypes.Structure):
    _fields_ = [
        ("wVk", wintypes.WORD),
        ("wScan", wintypes.WORD),
        ("dwFlags", wintypes.DWORD),
        ("time", wintypes.DWORD),
        ("dwExtraInfo", ctypes.POINTER(ctypes.c_ulong)),
    ]


class HardwareInput(ctypes.Structure):
    _fields_ = [
        ("uMsg", wintypes.DWORD),
        ("wParamL", wintypes.WORD),
        ("wParamH", wintypes.WORD),
    ]


class InputUnion(ctypes.Union):
    _fields_ = [("mi", MouseInput), ("ki", KeybdInput), ("hi", HardwareInput)]


class Input(ctypes.Structure):
    _fields_ = [("type", wintypes.DWORD), ("union", InputUnion)]


def click(x, y):
    user32.SetCursorPos(x, y)
    time.sleep(0.25)
    user32.mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, 0)
    time.sleep(0.06)
    user32.mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, 0)
    time.sleep(0.25)


def type_text(text):
    """逐字符注入 Unicode，不依赖键盘布局。"""
    for character in text:
        for flags in (KEYEVENTF_UNICODE, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP):
            item = Input()
            item.type = INPUT_KEYBOARD
            item.union.ki = KeybdInput(0, ord(character), flags, 0, None)
            user32.SendInput(1, ctypes.byref(item), ctypes.sizeof(Input))
        time.sleep(0.02)


VK_CONTROL = 0x11
VK_A = 0x41


def press_combo(modifier, key):
    """发一次组合键（先按 modifier 再按 key，然后逆序松开）。"""
    steps = ((modifier, 0), (key, 0), (key, KEYEVENTF_KEYUP), (modifier, KEYEVENTF_KEYUP))
    for vk, flags in steps:
        item = Input()
        item.type = INPUT_KEYBOARD
        item.union.ki = KeybdInput(vk, 0, flags, 0, None)
        user32.SendInput(1, ctypes.byref(item), ctypes.sizeof(Input))
        time.sleep(0.03)


def select_all():
    """Ctrl+A。脚本可能被反复运行，输入框里还留着上一轮的路径，必须先清掉，
    否则新路径会被追加到旧路径后面。"""
    press_combo(VK_CONTROL, VK_A)


# ---------------------------------------------------------------- 主流程


def main():
    if len(sys.argv) < 3:
        print(__doc__)
        return 2

    output = sys.argv[1]
    root = sys.argv[2]
    waits = [int(value) for value in sys.argv[3:]] or [20]

    user32.SetProcessDPIAware()

    hwnd, title = find_window("cefscanw")
    if hwnd is None:
        print("找不到 cefscanw 窗口，先把它跑起来")
        return 1
    print(f"窗口: {title!r} hwnd={hwnd}")
    focus(hwnd)

    width, _height, pixels = capture()
    region = window_region(hwnd)
    print(f"窗口矩形: {region}")

    button = find_button(pixels, width, region)
    if button is None:
        print("找不到强调色按钮")
        return 1
    button_x, button_y, box = button
    print(f"开始扫描按钮: 中心=({button_x},{button_y}) 外接矩形={box}")

    field = find_field(pixels, width, button_y, region, box[0] - 8)
    if field is None:
        print("找不到输入框")
        return 1
    field_x, run = field
    print(f"输入框: 中心x={field_x} 宽度={run}px")

    click(field_x, button_y)
    time.sleep(0.2)
    select_all()
    type_text(root)
    print(f"已输入目录: {root}")
    time.sleep(0.4)

    click(button_x, button_y)
    print("已点击扫描")

    elapsed = 0
    for wait in waits:
        time.sleep(max(wait - elapsed, 0))
        elapsed = wait
        width, height, pixels = capture()
        target = output.replace(".png", f"_t{wait}s.png")
        save_png(target, width, pixels, (0, 0, width, height))
        print(f"t={wait}s 已截屏 -> {target}")

    if os.environ.get("CEFSCAN_SMOKE_EXPAND"):
        width, _height, pixels = capture()
        row = find_first_row(pixels, width, region)
        if row is None:
            print("找不到数据行，跳过展开验证")
            return 1
        print(f"点第一行数据行: {row}")
        click(*row)
        time.sleep(0.8)
        width, height, pixels = capture()
        target = output.replace(".png", "_expanded.png")
        save_png(target, width, pixels, (0, 0, width, height))
        print(f"展开后已截屏 -> {target}")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
