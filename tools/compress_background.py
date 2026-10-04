"""把背景图从无损 WebP 转成有损 WebP q85。"""

from __future__ import annotations

import shutil
import sys
from pathlib import Path

from PIL import Image, ImageChops

QUALITY = 85
METHOD = 6  # 0~6，越大越慢压得越好。

REPO = Path(__file__).resolve().parent.parent
TARGET = REPO / "crates/cefscan-desktop/ui/assets/images/background.webp"
BACKUP = REPO / ".workbuddy-ai/assets-backup/background-lossless.webp"


def psnr(a: Image.Image, b: Image.Image, *, luma_only: bool = True) -> float:
    """峰值信噪比，默认只算亮度通道。"""
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

    # 已经转过就跳过。
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
    # alpha 全不透明就丢掉通道。
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
