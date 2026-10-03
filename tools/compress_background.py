"""把背景图从无损 WebP 转成有损 WebP q85。

为什么值得做：Tauri 的 `frontendDist` 是**原样嵌入**——`tauri-codegen` 把 ui/ 下的每个
文件当成字节数组塞进 exe，不做任何二次压缩。所以这张图多大，exe 就白白大多少。
736 KB 的无损图占了 5.73 MB exe 的 13%，而它只是个背景。

实测（1000x749 RGB，渐变+文字的海报）：

    无损 WebP   736.0 KB   ← 原方案
    WebP q90    157.0 KB
    WebP q85    122.4 KB   ← 本脚本
    WebP q80    100.5 KB
    PNG 24bit   917.6 KB   ← 反而更大，渐变+文字压不动
    JPEG q90    279.2 KB   ← 文字边缘有振铃，且比 WebP 大一倍

先备份原图再覆盖：有损是不可逆的，而原图**没有进 git**（.gitignore 里没有它，
但它是新加的，还没被 track），丢了就只能重新找素材。
"""

from __future__ import annotations

import shutil
import sys
from pathlib import Path

from PIL import Image, ImageChops

QUALITY = 85
METHOD = 6  # 0~6，越大越慢压得越好；离线转一次，直接拉满。

REPO = Path(__file__).resolve().parent.parent
TARGET = REPO / "crates/cefscan-desktop/ui/assets/images/background.webp"
BACKUP = REPO / ".workbuddy-ai/assets-backup/background-lossless.webp"


def psnr(a: Image.Image, b: Image.Image, *, luma_only: bool = True) -> float:
    """峰值信噪比。

    默认只算**亮度**通道，这不是偷懒：直接算 RGB 的 PSNR 在这张图上会给出严重偏低
    的数字（q85 只有 31.6 dB，看着像"明显劣化"），但拆开看是

        q85  亮度 Y 40.36 dB   Cb 34.51 dB   Cr 35.30 dB

    原因是 RGB 的 PSNR 把色度误差按和亮度一样的权重摊进来了，而人眼对色度的分辨率
    低得多（这正是 JPEG/WebP 敢对色度做 4:2:0 抽样的前提）。这张图又是高饱和的红金
    配色，色度误差天然大。所以 40 dB 的亮度信噪比才是和观感对得上的那个数——
    2 倍放大逐块比对也确实看不出差别。

    顺带记一条：`save(..., subsampling="4:4:4")` 对 WebP 是**无效参数**，Pillow 会
    静默忽略（字节数和 PSNR 与默认完全相同）。别指望用它来提画质。
    """
    import math

    diff = ImageChops.difference(a.convert("RGB"), b.convert("RGB"))
    if luma_only:
        diff = diff.convert("YCbCr").getchannel("Y")
    hist = diff.histogram()
    # histogram() 对多通道是各通道 256 桶拼接，均方误差按全部采样点算。
    samples = diff.size[0] * diff.size[1] * len(diff.getbands())
    mse = sum(v * (i % 256) ** 2 for i, v in enumerate(hist)) / samples
    if mse == 0:
        return float("inf")
    return 10 * math.log10((255**2) / mse)


def main() -> int:
    if not TARGET.exists():
        print(f"找不到 {TARGET}", file=sys.stderr)
        return 1

    original_bytes = TARGET.stat().st_size

    # 已经转过就别再转一次——有损再压一遍会累积劣化。
    if b"VP8L" not in TARGET.read_bytes()[:16]:
        print(f"{TARGET.name} 已经不是无损 WebP 了，跳过（避免二次有损劣化）")
        return 0

    BACKUP.parent.mkdir(parents=True, exist_ok=True)
    if not BACKUP.exists():
        shutil.copy2(TARGET, BACKUP)
        print(f"原图已备份 → {BACKUP.relative_to(REPO)}  ({original_bytes:,} 字节)")
    else:
        print(f"备份已存在，不覆盖 → {BACKUP.relative_to(REPO)}")

    src = Image.open(TARGET)
    print(f"源：{src.size[0]}x{src.size[1]} {src.mode}  {original_bytes:,} 字节")

    rgb = src.convert("RGB")
    # alpha 全不透明就直接丢掉通道，RGB 比 RGBA 少 25% 的原始数据。
    if src.mode in ("RGBA", "LA") and src.getchannel("A").getextrema() == (255, 255):
        print("alpha 通道全不透明，转成 RGB")

    tmp = TARGET.with_suffix(".tmp.webp")
    rgb.save(tmp, "WEBP", quality=QUALITY, method=METHOD)
    new_bytes = tmp.stat().st_size

    # 回读校验：尺寸必须一致，画质用 PSNR 量化存档。
    check = Image.open(tmp)
    if check.size != src.size:
        tmp.unlink()
        print(f"尺寸变了（{check.size} != {src.size}），已放弃", file=sys.stderr)
        return 1

    score = psnr(rgb, check)
    print(f"q{QUALITY} method={METHOD}：{new_bytes:,} 字节  ({new_bytes / 1024:.1f} KB)")
    print(f"省下 {original_bytes - new_bytes:,} 字节（-{(1 - new_bytes / original_bytes) * 100:.1f}%）")
    print(f"亮度 PSNR {score:.2f} dB（40 dB 以上观感上就分不出来了）")

    tmp.replace(TARGET)
    print(f"已写入 {TARGET.relative_to(REPO)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
