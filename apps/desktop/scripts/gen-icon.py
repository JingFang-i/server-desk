#!/usr/bin/env python3
"""生成 ServerDesk 应用图标（可复现，不依赖任何设计软件）。

设计说明
--------
概念：**终端提示符**。产品是通过 AI 在服务器上执行命令，主形就是行首提示符 `>` 与
一道青色光标；不做任何装饰性底纹——小尺寸（16px）下只有一个干净剪影，这是图标
能不能用的分水岭。前几版试过「三条机架槽位」的底纹，16–128px 会被读成一块文字，
已删除。

工艺：
- 4× 超采样（4096²）后 LANCZOS 降采样，边缘在每个尺寸都干净
- 超椭圆（n=5）做底板，接近 macOS Big Sur 的连续曲率圆角，不是普通圆角矩形
- 底色为 45° 双色渐变 + 顶部 1.2% 白高光，靠光影而不是描边做厚度
- 全部尺寸从同一份矢量色彩定义生成，避免各尺寸观感不一致

用法：python3 apps/desktop/scripts/gen-icon.py
"""

from __future__ import annotations

import math
import shutil
import subprocess
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw

# ---------------------------------------------------------------- 画布与配色

SS = 4  # 超采样倍数
BASE = 1024  # 逻辑尺寸
S = BASE * SS

# 底板渐变（左上 → 右下）
TOP = np.array([27, 37, 69], dtype=float)  # #1B2545
BOTTOM = np.array([9, 13, 26], dtype=float)  # #090D1A
ACCENT = (34, 211, 238)  # #22D3EE 青色光标
WHITE = (255, 255, 255)

TILE_INSET = 0.085  # 底板上、下留白（macOS 图标惯例）
CORNER_N = 5.0  # 超椭圆指数：2=椭圆，越大越方

ICON_DIR = Path(__file__).resolve().parent.parent / "src-tauri" / "icons"


def px(v: float) -> int:
    return int(round(v * S))


# ---------------------------------------------------------------- 底板


def squircle_mask(size: int, inset: float, n: float) -> Image.Image:
    """超椭圆遮罩：|x/a|^n + |y/b|^n <= 1，接近 macOS 图标的连续曲率。"""
    half = 0.5 - inset
    ys, xs = np.mgrid[0:size, 0:size]
    # 归一化到 [-1, 1]，再按半宽缩放
    x = (xs + 0.5) / size * 2 - 1
    y = (ys + 0.5) / size * 2 - 1
    x = np.abs(x) / (half * 2)
    y = np.abs(y) / (half * 2)
    inside = (x**n + y**n) <= 1.0
    alpha = np.where(inside, 255, 0).astype(np.uint8)
    return Image.fromarray(alpha, mode="L")


def diagonal_gradient(size: int, top: np.ndarray, bottom: np.ndarray) -> Image.Image:
    """45° 线性渐变（左上 → 右下）。"""
    ys, xs = np.mgrid[0:size, 0:size]
    t = (xs + ys) / (2.0 * (size - 1))  # 0..1
    t = t[..., None]
    rgb = top * (1 - t) + bottom * t
    return Image.fromarray(rgb.astype(np.uint8), mode="RGB")


def rounded_bar(draw: ImageDraw.ImageDraw, x0, y0, x1, y1, radius, fill):
    draw.rounded_rectangle([px(x0), px(y0), px(x1), px(y1)], radius=px(radius), fill=fill)


def stroke_path(points, width: float, fill, prev: Image.Image) -> Image.Image:
    """把折线画成粗笔画：线段 + 端点/拐点圆帽（Pillow 的 line 没有圆帽）。"""
    layer = Image.new("RGBA", prev.size, (0, 0, 0, 0))
    d = ImageDraw.Draw(layer)
    r = width / 2
    for (x0, y0), (x1, y1) in zip(points, points[1:]):
        d.line([px(x0), px(y0), px(x1), px(y1)], fill=fill, width=px(width))
    for x, y in points:
        d.ellipse([px(x - r), px(y - r), px(x + r), px(y + r)], fill=fill)
    return Image.alpha_composite(prev, layer)


def build_master() -> Image.Image:
    tile = diagonal_gradient(S, TOP, BOTTOM).convert("RGBA")
    mask = squircle_mask(S, TILE_INSET, CORNER_N)
    tile.putalpha(mask)

    # 顶部柔和高光：让底板像一块被光扫过的玻璃，而不是纯色块
    glow = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    gd = ImageDraw.Draw(glow)
    for i in range(px(0.30)):
        a = int(26 * (1 - i / px(0.30)) ** 2)
        gd.rectangle([0, i, S, i], fill=(255, 255, 255, a))
    glow.putalpha(Image.composite(glow.getchannel("A"), Image.new("L", (S, S), 0), mask))
    tile = Image.alpha_composite(tile, glow)

    layer = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    d = ImageDraw.Draw(layer)

    # 主形：提示符 ">"（光学居中：主形包围盒的中点在 (0.5, 0.5) 稍上方）
    ink = WHITE + (243,)
    layer = stroke_path(
        [(0.3125, 0.336), (0.5125, 0.496), (0.3125, 0.656)], 0.09, ink, layer
    )

    # 光标下划线：青色，厚度略小于笔画，与 ">" 的开口端呼应
    d = ImageDraw.Draw(layer)
    rounded_bar(d, 0.5725, 0.611, 0.7325, 0.686, 0.0375, ACCENT + (255,))

    layer.putalpha(Image.composite(layer.getchannel("A"), Image.new("L", (S, S), 0), mask))
    tile = Image.alpha_composite(tile, layer)

    # 底部内侧暗部：让底板看起来是块有厚度的实体，而不是一张贴纸
    shade = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    sd = ImageDraw.Draw(shade)
    band = int(0.16 * S)
    for i in range(band):
        a = int(22 * (i / band) ** 1.6)
        sd.rectangle([0, S - 1 - i, S, S - 1 - i], fill=(0, 0, 0, a))
    shade.putalpha(Image.composite(shade.getchannel("A"), Image.new("L", (S, S), 0), mask))
    return Image.alpha_composite(tile, shade)


# ---------------------------------------------------------------- 导出


def write_png(master: Image.Image, size: int, path: Path) -> None:
    master.resize((size, size), Image.LANCZOS).save(path, "PNG", optimize=True)
    print(f"  {path.name:<24} {size}×{size}")


def main() -> None:
    print("渲染主图（4096² 超采样）…")
    master = build_master()
    ICON_DIR.mkdir(parents=True, exist_ok=True)

    print("导出各尺寸 PNG：")
    sizes = {
        "32x32.png": 32,
        "64x64.png": 64,
        "128x128.png": 128,
        "128x128@2x.png": 256,
        "icon.png": 512,
        "StoreLogo.png": 50,
        "Square30x30Logo.png": 30,
        "Square44x44Logo.png": 44,
        "Square71x71Logo.png": 71,
        "Square89x89Logo.png": 89,
        "Square107x107Logo.png": 107,
        "Square142x142Logo.png": 142,
        "Square150x150Logo.png": 150,
        "Square284x284Logo.png": 284,
        "Square310x310Logo.png": 310,
    }
    for name, size in sizes.items():
        write_png(master, size, ICON_DIR / name)

    # .ico（Windows，多尺寸内嵌）
    ico = ICON_DIR / "icon.ico"
    master.resize((256, 256), Image.LANCZOS).save(
        ico, sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)]
    )
    print(f"  {'icon.ico':<24} 多尺寸")

    # .icns（macOS，先出 iconset 再 iconutil）
    iconset = ICON_DIR / "icon.iconset"
    if iconset.exists():
        shutil.rmtree(iconset)
    iconset.mkdir()
    for name, size in {
        "icon_16x16.png": 16,
        "icon_16x16@2x.png": 32,
        "icon_32x32.png": 32,
        "icon_32x32@2x.png": 64,
        "icon_128x128.png": 128,
        "icon_128x128@2x.png": 256,
        "icon_256x256.png": 256,
        "icon_256x256@2x.png": 512,
        "icon_512x512.png": 512,
        "icon_512x512@2x.png": 1024,
    }.items():
        master.resize((size, size), Image.LANCZOS).save(iconset / name, "PNG")
    subprocess.run(
        ["iconutil", "-c", "icns", str(iconset), "-o", str(ICON_DIR / "icon.icns")], check=True
    )
    shutil.rmtree(iconset)
    print(f"  {'icon.icns':<24} 16→1024")

    # 前端侧栏用的小图（与 App 图标同一份主图）
    front = ICON_DIR.parent.parent / "src" / "assets"
    front.mkdir(parents=True, exist_ok=True)
    write_png(master, 256, front / "logo.png")


if __name__ == "__main__":
    main()
