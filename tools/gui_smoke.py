#!/usr/bin/env python3
"""cefscanw 的端到端冒烟测试（Windows，仅手动运行，不进 `cargo test`）。"""

import argparse
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
VK_TAB = 0x09
VK_RETURN = 0x0D
VK_DOWN = 0x28

HWND_TOPMOST = -1
HWND_NOTOPMOST = -2
SWP_NOSIZE = 0x0001
SWP_NOMOVE = 0x0002
SWP_SHOWWINDOW = 0x0040

# 初始选择页的强调色（BGRA，即 :root 里的 --accent）。
PICKER_ACCENT = (247, 143, 79)  # #4f8ff7

TOLERANCE = 14
# 按钮最小宽度（px）。
BUTTON_MIN_WIDTH = 60
# 按钮最小高度（px）。
BUTTON_MIN_HEIGHT = 20
# 实心判据：强调色像素数 / 外接矩形面积。
BUTTON_MIN_FILL = 0.5

# KIND_COLORS（见 ui/main.js）里各标签底色，BGRA 顺序，用来定位表格数据行。
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
    """把窗口提到最前，并且硬性置顶。"""
    if user32.IsIconic(hwnd):
        user32.ShowWindow(hwnd, SW_RESTORE)
    user32.SetWindowPos(
        wintypes.HWND(hwnd),
        wintypes.HWND(HWND_TOPMOST),
        0,
        0,
        0,
        0,
        SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
    )
    user32.SetForegroundWindow(hwnd)
    time.sleep(0.6)


def unpin(hwnd):
    """取消置顶。"""
    user32.SetWindowPos(
        wintypes.HWND(hwnd),
        wintypes.HWND(HWND_NOTOPMOST),
        0,
        0,
        0,
        0,
        SWP_NOMOVE | SWP_NOSIZE,
    )


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


def find_solid_block(pixels, width, region, accent):
    """找强调色的实心方块，返回 ((中心 x, 中心 y), (left, top, right, bottom))。

    返回 `(None, None)` 表示没找到。
    """
    left, top, right, bottom = region

    hits = set()
    for y in range(top, bottom):
        row = y * width * 4
        for x in range(left, right):
            index = row + x * 4
            if close_to(pixels[index : index + 3], accent):
                hits.add((x, y))
    if not hits:
        return None, None

    # 8 邻接的连通域（flood fill）。
    best = None
    seen = set()
    for seed in hits:
        if seed in seen:
            continue
        seen.add(seed)
        stack = [seed]
        xs = []
        ys = []
        while stack:
            x, y = stack.pop()
            xs.append(x)
            ys.append(y)
            for dx in (-1, 0, 1):
                for dy in (-1, 0, 1):
                    if dx == 0 and dy == 0:
                        continue
                    neighbour = (x + dx, y + dy)
                    if neighbour in hits and neighbour not in seen:
                        seen.add(neighbour)
                        stack.append(neighbour)

        box = (min(xs), min(ys), max(xs), max(ys))
        box_width = box[2] - box[0] + 1
        box_height = box[3] - box[1] + 1
        fill = len(xs) / (box_width * box_height)
        if box_width < BUTTON_MIN_WIDTH or box_height < BUTTON_MIN_HEIGHT:
            continue
        print(f"  实心候选: x {box[0]}..{box[2]} 尺寸 {box_width}x{box_height} 填充率 {fill:.2f}")
        if fill < BUTTON_MIN_FILL:
            continue
        area = box_width * box_height
        if best is None or area > best[0]:
            best = (area, box)

    if best is None:
        return None, None

    _, box = best
    print(f"  采用最大实心块: x {box[0]}..{box[2]} y {box[1]}..{box[3]}")
    return ((box[0] + box[2]) // 2, (box[1] + box[3]) // 2), box


def find_first_row(pixels, width, region, min_hits=40):
    """找表格第一条数据行，用「类型」列的标签底色定位。

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
    """逐字符注入 Unicode。"""
    for character in text:
        for flags in (KEYEVENTF_UNICODE, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP):
            item = Input()
            item.type = INPUT_KEYBOARD
            item.union.ki = KeybdInput(0, ord(character), flags, 0, None)
            user32.SendInput(1, ctypes.byref(item), ctypes.sizeof(Input))
        time.sleep(0.02)


def press_key(vk):
    """敲一次普通按键（按下 + 抬起）。"""
    for flags in (0, KEYEVENTF_KEYUP):
        item = Input()
        item.type = INPUT_KEYBOARD
        item.union.ki = KeybdInput(vk, 0, flags, 0, None)
        user32.SendInput(1, ctypes.byref(item), ctypes.sizeof(Input))
        time.sleep(0.03)
    time.sleep(0.1)


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
    """Ctrl+A。"""
    press_combo(VK_CONTROL, VK_A)


# ---------------------------------------------------------------- 主流程


def locate_start_button(hwnd, region, attempts=3):
    """抓帧并找「开始扫描」按钮，失败就重新聚焦再抓一次。"""
    for attempt in range(1, attempts + 1):
        width, _height, pixels = capture()
        button, box = find_solid_block(pixels, width, region, PICKER_ACCENT)
        if button is not None:
            return button, box, width, pixels
        if attempt < attempts:
            print(f"第 {attempt} 次没找到开始按钮，重新聚焦再抓一帧")
            focus(hwnd)
            time.sleep(0.8)
    return None, None, None, None


def choose_mode(mode):
    """在初始选择页把模式选好（键盘操作）。"""
    press_key(VK_TAB)
    if mode == "tool":
        press_key(VK_DOWN)
    time.sleep(0.25)


def type_root_and_rescan(root):
    """工具模式专用：把路径打进工具栏输入框并回车重扫。"""
    press_key(VK_TAB)  # 返回按钮
    press_key(VK_TAB)  # 目录输入框
    select_all()
    type_text(root)
    time.sleep(0.3)
    press_key(VK_RETURN)


def parse_args(argv):
    parser = argparse.ArgumentParser(
        description="cefscanw 端到端冒烟测试",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("output", help="截图输出路径（.png）")
    parser.add_argument("times", nargs="*", type=int, help="截屏时刻（秒），可给多个")
    parser.add_argument(
        "--mode",
        choices=("classic", "tool"),
        default=os.environ.get("CEFSCAN_SMOKE_MODE", "classic"),
        help="在初始选择页选哪个模式（默认 classic，也可用 CEFSCAN_SMOKE_MODE 指定）",
    )
    parser.add_argument(
        "--root",
        default=None,
        help="工具模式下额外限定一次目录并重扫（经典模式没有输入框，会被忽略）",
    )
    return parser.parse_args(argv)


def main(argv=None):
    args = parse_args(argv if argv is not None else sys.argv[1:])
    output = args.output
    waits = args.times or [20]

    user32.SetProcessDPIAware()

    hwnd, title = find_window("cefscanw")
    if hwnd is None:
        print("找不到 cefscanw 窗口，先把它跑起来")
        return 1
    print(f"窗口: {title!r} hwnd={hwnd}")
    focus(hwnd)

    region = window_region(hwnd)
    print(f"窗口矩形: {region}")

    # ① 初始选择页：先确认它真的画出来了（按钮在不在、是不是实心块）。
    button, box, width, pixels = locate_start_button(hwnd, region)
    if button is None:
        print("找不到选择页的「开始扫描」按钮（强调色实心块，重试过抓帧）")
        unpin(hwnd)
        return 1
    button_x, button_y = button
    print(f"开始扫描按钮: 中心=({button_x},{button_y}) 外接矩形={box}")

    # ② 选模式，然后真点一下按钮。
    choose_mode(args.mode)
    print(f"已选择模式: {args.mode}")
    click(button_x, button_y)
    print("已点击开始扫描")

    # ③ 定时截屏。
    elapsed = 0
    for wait in waits:
        time.sleep(max(wait - elapsed, 0))
        elapsed = wait
        width, height, pixels = capture()
        target = output.replace(".png", f"_t{wait}s.png")
        save_png(target, width, pixels, (0, 0, width, height))
        print(f"t={wait}s 已截屏 -> {target}")

    # ④ 工具模式可选：限定目录再扫一遍，验证输入框 + 回车重扫这条链路。
    if args.root:
        if args.mode != "tool":
            print("经典模式没有目录输入框，--root 被忽略（第一轮已经是全盘扫描）")
        else:
            print(f"在工具栏输入目录并重扫: {args.root}")
            type_root_and_rescan(args.root)
            time.sleep(3)
            width, height, pixels = capture()
            target = output.replace(".png", "_filtered.png")
            save_png(target, width, pixels, (0, 0, width, height))
            print(f"限定目录后已截屏 -> {target}")

    # ⑤ 工具模式可选：点第一行数据行，验证整行展开。
    if os.environ.get("CEFSCAN_SMOKE_EXPAND"):
        if args.mode != "tool":
            print("经典模式里点卡片是在资源管理器里定位，跳过展开验证")
        else:
            width, _height, pixels = capture()
            row = find_first_row(pixels, width, region)
            if row is None:
                print("找不到数据行，跳过展开验证")
                unpin(hwnd)
                return 1
            print(f"点第一行数据行: {row}")
            click(*row)
            time.sleep(0.8)
            width, height, pixels = capture()
            target = output.replace(".png", "_expanded.png")
            save_png(target, width, pixels, (0, 0, width, height))
            print(f"展开后已截屏 -> {target}")

    unpin(hwnd)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
