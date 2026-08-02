#!/usr/bin/env python3
"""Generate the original OpenCode2API Mecha Control Deck pixel-art asset pack.

The production UI keeps all dynamic text/data in HTML. This generator produces only
backgrounds, textures, frames, icons, mascots and decorative illustrations.
"""
from __future__ import annotations

from pathlib import Path
from typing import Callable
import math
import random

from PIL import Image, ImageDraw, ImageFilter

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "src" / "webui" / "assets" / "mecha"
PREVIEW = ROOT / "artifacts" / "mecha-control-deck" / "asset-preview-board.png"

PALETTE = {
    "canvas": "#080B1A",
    "sidebar": "#0D1025",
    "header": "#10142B",
    "surface": "#14182F",
    "surface_hover": "#1B2040",
    "surface_active": "#242957",
    "primary": "#8B6CFF",
    "primary_hover": "#A18BFF",
    "primary_dark": "#5B43C9",
    "pink": "#FF8FCF",
    "cyan": "#5CCBFF",
    "success": "#55E6A5",
    "warning": "#FFBD68",
    "error": "#FF6F91",
    "text": "#F8F7FF",
    "muted": "#777A9C",
    "border": "#343963",
    "border_subtle": "#24284B",
}

random.seed(20260722)


def rgba(hex_color: str, alpha: int = 255) -> tuple[int, int, int, int]:
    value = hex_color.lstrip("#")
    return tuple(int(value[i:i+2], 16) for i in (0, 2, 4)) + (alpha,)


def ensure_dirs() -> None:
    for name in [
        "branding", "backgrounds", "frames", "icons", "status", "metrics",
        "mascot", "buttons", "states",
    ]:
        (OUT / name).mkdir(parents=True, exist_ok=True)
    PREVIEW.parent.mkdir(parents=True, exist_ok=True)


def save(img: Image.Image, path: Path, **kwargs) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.suffix.lower() == ".webp":
        img.save(path, "WEBP", lossless=True, quality=88, method=6, **kwargs)
    else:
        img.save(path, **kwargs)


def pixel_upscale(img: Image.Image, scale: int) -> Image.Image:
    return img.resize((img.width * scale, img.height * scale), Image.Resampling.NEAREST)


def logo_mark(size: int, background: bool = True, monochrome: bool = False) -> Image.Image:
    base = Image.new("RGBA", (32, 32), (0, 0, 0, 0))
    d = ImageDraw.Draw(base)
    bg = rgba(PALETTE["surface"], 245)
    if background:
        d.rounded_rectangle((1, 1, 30, 30), radius=6, fill=bg, outline=rgba(PALETTE["border"], 255), width=1)
        d.line((5, 3, 27, 3), fill=rgba(PALETTE["cyan"], 155), width=1)
        d.line((4, 28, 16, 28), fill=rgba(PALETTE["primary"], 130), width=1)
    c1 = rgba(PALETTE["text"] if monochrome else PALETTE["primary"])
    c2 = rgba(PALETTE["text"] if monochrome else PALETTE["cyan"])
    # API gateway brackets.
    d.line((9, 8, 4, 16, 9, 24), fill=c1, width=3)
    d.line((23, 8, 28, 16, 23, 24), fill=c1, width=3)
    # Core route nodes.
    d.line((10, 16, 22, 16), fill=c2, width=2)
    for x in (10, 16, 22):
        d.rectangle((x - 2, 14, x + 2, 18), fill=c2)
    d.line((16, 12, 16, 9, 20, 9), fill=c1, width=1)
    d.line((16, 20, 16, 23, 12, 23), fill=c1, width=1)
    return base.resize((size, size), Image.Resampling.NEAREST)


def generate_branding() -> None:
    save(logo_mark(256, True), OUT / "branding" / "logo-icon.png")
    # Primary lockup remains image-only: no rendered product name.
    mark = logo_mark(96, True)
    canvas = Image.new("RGBA", (320, 112), (0, 0, 0, 0))
    canvas.alpha_composite(mark, (8, 8))
    d = ImageDraw.Draw(canvas)
    # Decorative energy rails only; live brand text remains HTML.
    d.rectangle((114, 30, 304, 36), fill=rgba(PALETTE["border_subtle"], 255))
    d.rectangle((114, 32, 252, 34), fill=rgba(PALETTE["cyan"], 190))
    d.rectangle((114, 54, 276, 62), fill=rgba(PALETTE["border"], 180))
    d.rectangle((114, 56, 224, 60), fill=rgba(PALETTE["primary"], 220))
    d.rectangle((114, 78, 196, 82), fill=rgba(PALETTE["pink"], 150))
    save(canvas, OUT / "branding" / "logo-primary.png")
    for size in (16, 32, 64):
        save(logo_mark(size, True), OUT / "branding" / f"favicon-{size}.png")
    save(logo_mark(512, True), OUT / "branding" / "app-icon-512.png")
    monochrome_svg = '''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32" fill="none" stroke="currentColor" stroke-linecap="square" stroke-linejoin="miter"><path d="M9 8 4 16l5 8M23 8l5 8-5 8" stroke-width="3"/><path d="M10 16h12M16 16V9h4M16 16v7h-4" stroke-width="2"/><rect x="8" y="14" width="4" height="4" fill="currentColor" stroke="none"/><rect x="14" y="14" width="4" height="4" fill="currentColor" stroke="none"/><rect x="20" y="14" width="4" height="4" fill="currentColor" stroke="none"/></svg>'''
    (OUT / "branding" / "logo-monochrome.svg").write_text(monochrome_svg)


def draw_hud_background(size: tuple[int, int], mobile: bool = False, variant: str = "app") -> Image.Image:
    w, h = size
    scale = 4 if w >= 1000 else 2
    lw, lh = max(1, w // scale), max(1, h // scale)
    img = Image.new("RGBA", (lw, lh), rgba(PALETTE["canvas"]))
    d = ImageDraw.Draw(img)
    # Radial-looking pixel blocks and edge glow.
    for y in range(lh):
        mix = y / max(1, lh - 1)
        r0, g0, b0, _ = rgba(PALETTE["canvas"])
        r1, g1, b1, _ = rgba(PALETTE["surface"])
        col = (int(r0 + (r1-r0)*mix*.45), int(g0 + (g1-g0)*mix*.45), int(b0 + (b1-b0)*mix*.45), 255)
        d.line((0, y, lw, y), fill=col)
    grid = rgba(PALETTE["border_subtle"], 70 if mobile else 88)
    step = 16 if mobile else 20
    for x in range(0, lw, step):
        d.line((x, 0, x, lh), fill=grid)
    for y in range(0, lh, step):
        d.line((0, y, lw, y), fill=grid)
    # Mecha control-room edge consoles. Keep centre quiet.
    accents = [PALETTE["primary"], PALETTE["cyan"], PALETTE["pink"]]
    for idx, color in enumerate(accents):
        c = rgba(color, 105 - idx * 15)
        inset = 5 + idx * 5
        d.line((inset, inset, lw // 4, inset), fill=c, width=1)
        d.line((inset, inset, inset, lh // 4), fill=c, width=1)
        d.line((lw - inset, lh - inset, lw * 3 // 4, lh - inset), fill=c, width=1)
        d.line((lw - inset, lh - inset, lw - inset, lh * 3 // 4), fill=c, width=1)
    if variant == "sidebar":
        d.rectangle((0, 0, lw - 1, lh - 1), outline=rgba(PALETTE["primary_dark"], 85))
        for y in range(12, lh, 30):
            d.rectangle((3, y, 6, y + 14), fill=rgba(PALETTE["cyan"], 30))
    elif variant == "header":
        d.line((0, lh - 2, lw, lh - 2), fill=rgba(PALETTE["cyan"], 105), width=1)
        for x in range(10, lw, 38):
            d.rectangle((x, 3, x + 14, 4), fill=rgba(PALETTE["primary"], 55))
    elif variant == "control-room":
        # Console silhouettes at bottom and side.
        d.polygon([(0, lh), (0, lh*3//5), (lw//8, lh*2//3), (lw//5, lh)], fill=rgba("#050713", 235))
        d.polygon([(lw, lh), (lw, lh*3//5), (lw*7//8, lh*2//3), (lw*4//5, lh)], fill=rgba("#050713", 235))
        d.rectangle((lw//4, lh*4//5, lw*3//4, lh), fill=rgba("#070A18", 245))
        for x in range(lw//4 + 8, lw*3//4 - 8, 20):
            d.rectangle((x, lh*4//5 + 5, x + 10, lh*4//5 + 8), fill=rgba(PALETTE["cyan"], 85))
    # Sparse stars.
    for _ in range(max(20, (lw * lh) // 850)):
        x, y = random.randrange(lw), random.randrange(lh)
        if lw // 5 < x < lw * 4 // 5 and lh // 6 < y < lh * 5 // 6:
            continue
        d.point((x, y), fill=rgba(random.choice(accents), random.randrange(45, 130)))
    return img.resize(size, Image.Resampling.NEAREST)


def generate_backgrounds() -> None:
    save(draw_hud_background((1920, 1080)), OUT / "backgrounds" / "bg-app.webp")
    save(draw_hud_background((480, 960), True), OUT / "backgrounds" / "bg-app-mobile.webp")
    save(draw_hud_background((512, 1024), variant="sidebar"), OUT / "backgrounds" / "bg-sidebar.webp")
    save(draw_hud_background((1920, 160), variant="header"), OUT / "backgrounds" / "bg-header.webp")
    save(draw_hud_background((1600, 900), variant="control-room"), OUT / "backgrounds" / "bg-control-room.webp")
    save(draw_hud_background((480, 720), True, "control-room"), OUT / "backgrounds" / "bg-control-room-mobile.webp")
    # Grid texture.
    grid = Image.new("RGBA", (64, 64), (0, 0, 0, 0))
    gd = ImageDraw.Draw(grid)
    gd.line((0, 0, 63, 0), fill=rgba(PALETTE["primary"], 24))
    gd.line((0, 0, 0, 63), fill=rgba(PALETTE["cyan"], 24))
    gd.point((32, 32), fill=rgba(PALETTE["text"], 24))
    save(grid, OUT / "backgrounds" / "bg-grid-texture.png")
    stars = Image.new("RGBA", (128, 128), (0, 0, 0, 0))
    sd = ImageDraw.Draw(stars)
    for _ in range(34):
        sd.point((random.randrange(128), random.randrange(128)), fill=rgba(random.choice([PALETTE["cyan"], PALETTE["primary"], PALETTE["text"]]), random.randrange(20, 90)))
    save(stars, OUT / "backgrounds" / "bg-stars-texture.png")
    noise = Image.new("RGBA", (64, 64), (0, 0, 0, 0))
    nd = ImageDraw.Draw(noise)
    for y in range(64):
        for x in range(64):
            if random.random() < 0.08:
                nd.point((x, y), fill=(255, 255, 255, random.randrange(2, 10)))
    save(noise, OUT / "backgrounds" / "bg-panel-noise.png")
    save(draw_hud_background((960, 540), variant="control-room"), OUT / "backgrounds" / "bg-empty-state.webp")


def frame_image(kind: str, size: int = 64) -> Image.Image:
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    color_map = {
        "default": PALETTE["border"], "active": PALETTE["primary"],
        "danger": PALETTE["error"], "modal": PALETTE["cyan"],
        "tooltip": PALETTE["pink"], "sidebar": PALETTE["primary"],
    }
    c = rgba(color_map[kind], 220)
    c2 = rgba(PALETTE["surface"], 235)
    # Nine-slice-friendly frame: inset 16.
    d.rectangle((8, 8, size - 9, size - 9), fill=c2)
    d.line((16, 4, size - 17, 4), fill=c, width=2)
    d.line((16, size - 5, size - 17, size - 5), fill=c, width=2)
    d.line((4, 16, 4, size - 17), fill=c, width=2)
    d.line((size - 5, 16, size - 5, size - 17), fill=c, width=2)
    for x, y, sx, sy in [(4, 4, 1, 1), (size-5, 4, -1, 1), (4, size-5, 1, -1), (size-5, size-5, -1, -1)]:
        d.line((x, y + 6*sy, x + 6*sx, y), fill=c, width=2)
        d.line((x + 6*sx, y, x + 12*sx, y), fill=c, width=2)
    d.rectangle((10, 10, size - 11, size - 11), outline=rgba(PALETTE["border_subtle"], 255))
    return img


def generate_frames() -> None:
    mapping = {
        "frame-card-default.9.png": "default",
        "frame-card-active.9.png": "active",
        "frame-card-danger.9.png": "danger",
        "frame-modal.9.png": "modal",
        "frame-tooltip.9.png": "tooltip",
        "frame-sidebar-active.9.png": "sidebar",
    }
    for filename, kind in mapping.items():
        save(frame_image(kind), OUT / "frames" / filename)
    h = Image.new("RGBA", (128, 4), (0, 0, 0, 0)); hd = ImageDraw.Draw(h)
    hd.line((0, 1, 127, 1), fill=rgba(PALETTE["border"], 150)); hd.point((32, 1), fill=rgba(PALETTE["cyan"])); hd.point((96, 1), fill=rgba(PALETTE["primary"]))
    save(h, OUT / "frames" / "divider-horizontal.png")
    save(h.rotate(90, expand=True), OUT / "frames" / "divider-vertical.png")
    for pos in ("tl", "tr", "bl", "br"):
        c = Image.new("RGBA", (32, 32), (0, 0, 0, 0)); cd = ImageDraw.Draw(c)
        cd.line((2, 18, 2, 2, 18, 2), fill=rgba(PALETTE["primary"], 190), width=2)
        cd.line((6, 22, 6, 6, 22, 6), fill=rgba(PALETTE["cyan"], 85), width=1)
        if "r" in pos: c = c.transpose(Image.Transpose.FLIP_LEFT_RIGHT)
        if "b" in pos: c = c.transpose(Image.Transpose.FLIP_TOP_BOTTOM)
        save(c, OUT / "frames" / f"corner-decoration-{pos}.png")
    (OUT / "frames" / "NINE_SLICE.md").write_text(
        "# Nine-slice frame usage\n\nOriginal frame size: 64×64 px.\nBorder inset: 16 px.\nCSS: `border-image: url(...) 16 fill / 16px / 0 stretch;`\n"
    )


def draw_icon(name: str, size: int, color: str) -> Image.Image:
    img = Image.new("RGBA", (32, 32), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    c = rgba(color)
    c2 = rgba(PALETTE["surface_active"], 220)
    # Pixelated glyph library.
    def line(points, width=2): d.line(points, fill=c, width=width, joint="curve")
    if name == "dashboard":
        for box in [(4,4,14,14),(18,4,28,11),(18,15,28,28),(4,18,14,28)]: d.rectangle(box, outline=c, width=2)
    elif name in ("api-keys", "key"):
        d.ellipse((4,12,14,22), outline=c, width=2); line((13,17,28,4),3); d.rectangle((23,4,28,9), outline=c, width=2)
    elif name in ("models", "model-core"):
        d.polygon([(16,3),(28,10),(28,22),(16,29),(4,22),(4,10)], outline=c); d.ellipse((11,11,21,21), fill=c2, outline=c, width=2); d.rectangle((14,14,18,18), fill=c)
    elif name == "history":
        d.arc((4,4,28,28), 40, 330, fill=c, width=3); line((4,5,4,12,11,12),2); line((16,9,16,17,22,20),2)
    elif name == "system":
        for y, x in [(7,21),(16,10),(25,23)]: line((4,y,28,y),2); d.rectangle((x-2,y-2,x+2,y+2), fill=c)
    elif name in ("refresh", "restart"):
        d.arc((5,5,27,27), 30, 205, fill=c, width=3); d.arc((5,5,27,27), 210, 385, fill=c, width=3); d.polygon([(25,5),(29,10),(22,10)], fill=c); d.polygon([(7,27),(3,22),(10,22)], fill=c)
    elif name == "search":
        d.ellipse((5,5,21,21), outline=c, width=3); line((20,20,28,28),3)
    elif name == "filter":
        d.polygon([(3,5),(29,5),(20,15),(20,26),(12,29),(12,15)], outline=c)
    elif name == "settings":
        d.ellipse((9,9,23,23), outline=c, width=3); d.rectangle((14,3,18,9), fill=c); d.rectangle((14,23,18,29), fill=c); d.rectangle((3,14,9,18), fill=c); d.rectangle((23,14,29,18), fill=c)
    elif name == "add":
        line((16,5,16,27),3); line((5,16,27,16),3)
    elif name == "edit":
        line((6,25,10,18,22,6,27,11,15,23,6,25),2)
    elif name == "delete":
        d.rectangle((8,10,24,27), outline=c, width=2); line((6,7,26,7),3); line((12,4,20,4),2)
    elif name == "copy":
        d.rectangle((10,10,27,27), outline=c, width=2); d.rectangle((5,5,22,22), outline=c, width=2)
    elif name in ("download", "upload"):
        line((16,4,16,21),3); direction = 1 if name == "download" else -1
        if direction == 1: d.polygon([(9,16),(16,24),(23,16)], fill=c)
        else: d.polygon([(9,10),(16,2),(23,10)], fill=c)
        line((5,27,27,27),3)
    elif name == "logout":
        d.rectangle((4,5,18,27), outline=c, width=2); line((13,16,29,16),3); d.polygon([(24,10),(30,16),(24,22)], fill=c)
    elif name == "more":
        for x in (7,16,25): d.rectangle((x-2,14,x+2,18), fill=c)
    elif name in ("play",):
        d.polygon([(9,5),(27,16),(9,27)], fill=c)
    elif name == "stop":
        d.rectangle((7,7,25,25), fill=c)
    elif name == "service":
        d.ellipse((12,12,20,20), fill=c); line((16,4,16,12),2); line((16,20,16,28),2); line((4,16,12,16),2); line((20,16,28,16),2)
    elif name in ("requests", "success-rate"):
        for x, top in [(5,18),(12,10),(19,14),(26,5)]: d.rectangle((x-2,top,x+1,28), fill=c)
        if name == "success-rate": line((4,15,10,21,22,8,28,12),2)
    elif name in ("uptime", "latency"):
        d.ellipse((5,5,27,27), outline=c, width=2); line((16,8,16,17,22,20),2)
    elif name == "memory":
        d.rectangle((7,7,25,25), outline=c, width=2); d.rectangle((11,11,21,21), fill=c2, outline=c); [d.rectangle((x,3,x+2,7),fill=c) for x in (9,15,21)]
    elif name in ("proxy", "worker"):
        for x,y in [(16,5),(6,25),(26,25)]: d.ellipse((x-3,y-3,x+3,y+3), fill=c)
        line((16,8,7,22),2); line((16,8,25,22),2); line((9,25,23,25),2)
    elif name == "circuit-breaker":
        line((4,16,11,16,14,9,18,23,21,16,28,16),2)
    else:
        d.rectangle((6,6,26,26), outline=c, width=2); d.rectangle((13,13,19,19), fill=c)
    return img.resize((size, size), Image.Resampling.NEAREST)


def generate_icons() -> None:
    icons = ["dashboard","api-keys","models","history","system","refresh","search","filter","settings","add","edit","delete","copy","download","upload","logout","more","play","restart","stop"]
    states = {
        "default": PALETTE["muted"],
        "hover": PALETTE["cyan"],
        "active": PALETTE["primary_hover"],
        "disabled": "#4C4F70",
    }
    for name in icons:
        for state, color in states.items():
            for size in (16, 20, 24, 32):
                save(draw_icon(name, size, color), OUT / "icons" / name / f"{state}-{size}.png")


def status_icon(kind: str, size: int = 32) -> Image.Image:
    colors = {
        "online": PALETTE["success"], "healthy": PALETTE["success"], "success": PALETTE["success"],
        "offline": PALETTE["muted"], "degraded": PALETTE["warning"], "warning": PALETTE["warning"],
        "error": PALETTE["error"], "connecting": PALETTE["cyan"], "processing": PALETTE["primary"],
        "locked": PALETTE["pink"], "unlocked": PALETTE["cyan"],
    }
    img = Image.new("RGBA", (32,32), (0,0,0,0)); d = ImageDraw.Draw(img); c = rgba(colors[kind])
    if kind in ("locked", "unlocked"):
        d.rectangle((8,14,24,27), outline=c, width=2)
        if kind == "locked": d.arc((10,5,22,19), 180, 360, fill=c, width=2)
        else: d.arc((12,5,26,19), 180, 320, fill=c, width=2)
        d.rectangle((15,18,17,23), fill=c)
    else:
        d.ellipse((5,5,27,27), outline=rgba(colors[kind],150), width=2)
        d.rectangle((12,12,20,20), fill=c)
        if kind in ("connecting","processing"):
            d.arc((2,2,30,30), 20, 125, fill=c, width=2)
        elif kind in ("warning","degraded"):
            d.polygon([(16,5),(28,27),(4,27)], outline=c); d.rectangle((15,11,17,20), fill=c); d.rectangle((15,23,17,25), fill=c)
        elif kind == "error":
            d.line((10,10,22,22), fill=c, width=3); d.line((22,10,10,22), fill=c, width=3)
        elif kind in ("healthy","success","online"):
            d.line((9,16,14,21,23,11), fill=c, width=3)
    return img.resize((size,size), Image.Resampling.NEAREST)


def generate_status_metrics() -> None:
    for name in ["online","offline","connecting","healthy","degraded","warning","error","success","processing","locked","unlocked"]:
        save(status_icon(name), OUT / "status" / f"{name}.png")
    for name in ["service","model-core","requests","uptime","success-rate","latency","memory","proxy","worker","circuit-breaker"]:
        save(draw_icon(name, 48, PALETTE["cyan"] if name in ("latency","proxy","worker") else PALETTE["primary_hover"]), OUT / "metrics" / f"{name}.png")


def aria_character(state: str, target: tuple[int,int]) -> Image.Image:
    # Draw at a compact pixel grid and upscale, ensuring original non-franchise design.
    bw, bh = 72, 96
    img = Image.new("RGBA", (bw,bh), (0,0,0,0)); d = ImageDraw.Draw(img)
    outline = rgba("#11152F")
    suit = rgba(PALETTE["primary_dark"])
    suit_hi = rgba(PALETTE["primary"])
    cyan = rgba(PALETTE["cyan"])
    pink = rgba(PALETTE["pink"])
    skin = rgba("#F4C7C3")
    hair = rgba("#D9DCFF")
    # Legs and boots.
    d.rectangle((25,66,34,88), fill=suit, outline=outline); d.rectangle((39,66,48,88), fill=suit, outline=outline)
    d.rectangle((21,86,34,92), fill=outline); d.rectangle((39,86,52,92), fill=outline)
    # Torso pilot suit.
    d.polygon([(23,40),(49,40),(56,66),(17,66)], fill=suit, outline=outline)
    d.polygon([(28,42),(44,42),(48,61),(24,61)], fill=suit_hi)
    d.rectangle((33,44,39,60), fill=cyan)
    d.rectangle((26,61,46,65), fill=pink)
    # Arms.
    d.line((21,46,11,64), fill=suit_hi, width=7); d.line((51,46,61,64), fill=suit_hi, width=7)
    d.rectangle((8,61,14,68), fill=skin); d.rectangle((58,61,64,68), fill=skin)
    # Head/helmet.
    d.rectangle((21,12,51,39), fill=hair, outline=outline)
    d.rectangle((18,16,23,33), fill=suit_hi); d.rectangle((49,16,54,33), fill=suit_hi)
    d.rectangle((24,19,48,36), fill=skin)
    d.rectangle((25,21,47,27), fill=rgba("#2A315F"))
    d.rectangle((27,22,45,25), fill=cyan)
    d.rectangle((33,28,39,30), fill=pink)
    # Headset antenna and product core.
    d.line((52,18,61,10), fill=cyan, width=2); d.rectangle((59,8,63,12), fill=pink)
    d.rectangle((33,48,39,54), fill=rgba(PALETTE["text"])); d.rectangle((35,50,37,52), fill=cyan)
    if state == "success":
        d.line((56,50,66,40), fill=rgba(PALETTE["success"]), width=4); d.rectangle((63,36,68,42), fill=rgba(PALETTE["success"]))
    elif state == "warning":
        d.polygon([(61,38),(69,53),(53,53)], fill=rgba(PALETTE["warning"])); d.rectangle((60,43,62,48), fill=outline)
    elif state == "error":
        d.line((56,42,68,54), fill=rgba(PALETTE["error"]), width=3); d.line((68,42,56,54), fill=rgba(PALETTE["error"]), width=3)
    elif state.startswith("loading"):
        phase = 1 if state.endswith("01") else 2
        for i in range(4):
            if i % 2 == phase % 2:
                d.rectangle((5 + i*7, 74, 9 + i*7, 78), fill=rgba(PALETTE["cyan"]))
    return img.resize(target, Image.Resampling.NEAREST)


def drone(state: str, size: int = 96) -> Image.Image:
    img = Image.new("RGBA", (48,48), (0,0,0,0)); d = ImageDraw.Draw(img)
    body = rgba(PALETTE["surface_active"]); edge = rgba(PALETTE["primary"])
    status = PALETTE["success"] if state == "connected" else PALETTE["error"] if state == "error" else PALETTE["cyan"]
    d.polygon([(24,5),(39,14),(39,31),(24,42),(9,31),(9,14)], fill=body, outline=edge)
    d.rectangle((15,17,33,28), fill=rgba("#090C1D"), outline=rgba(status))
    d.rectangle((21,20,27,25), fill=rgba(status))
    d.line((4,12,9,17), fill=edge, width=2); d.line((44,12,39,17), fill=edge, width=2)
    d.rectangle((2,9,6,14), fill=rgba(status)); d.rectangle((42,9,46,14), fill=rgba(status))
    return img.resize((size,size), Image.Resampling.NEAREST)


def mecha_silhouette(size: tuple[int,int]) -> Image.Image:
    bw,bh = 128,96
    img = Image.new("RGBA", (bw,bh), (0,0,0,0)); d = ImageDraw.Draw(img)
    dark = rgba("#050713", 240); edge = rgba(PALETTE["primary"], 165); cyan=rgba(PALETTE["cyan"],180)
    # Original broad-shouldered maintenance mecha.
    d.polygon([(51,14),(77,14),(83,25),(95,31),(91,55),(80,58),(77,85),(66,91),(62,63),(58,63),(55,91),(44,85),(47,58),(36,55),(32,31),(45,25)], fill=dark, outline=edge)
    d.polygon([(50,16),(64,8),(78,16),(74,28),(54,28)], fill=dark, outline=edge)
    d.rectangle((56,18,72,22), fill=cyan)
    d.rectangle((57,35,71,50), fill=rgba(PALETTE["surface_active"]), outline=edge)
    d.rectangle((62,39,66,46), fill=cyan)
    d.polygon([(34,31),(12,41),(9,61),(20,64),(39,50)], fill=dark, outline=edge)
    d.polygon([(94,31),(116,41),(119,61),(108,64),(89,50)], fill=dark, outline=edge)
    return img.resize(size, Image.Resampling.NEAREST)


def compose_state(kind: str, size=(480,270)) -> Image.Image:
    base = draw_hud_background(size, variant="control-room")
    art = aria_character("success" if "success" in kind else "error" if "failed" in kind or "error" in kind or "denied" in kind else "warning" if "disconnected" in kind else "idle", (144,192))
    mech = mecha_silhouette((260,195))
    if kind in ("empty-models","empty-proxy"):
        base.alpha_composite(mech, (size[0]-280, size[1]-200))
    else:
        base.alpha_composite(art, (size[0]-165, size[1]-200))
    d = ImageDraw.Draw(base)
    color = PALETTE["success"] if "success" in kind else PALETTE["error"] if "failed" in kind or "error" in kind or "denied" in kind else PALETTE["warning"] if "disconnected" in kind else PALETTE["cyan"]
    d.rectangle((24, size[1]-48, size[0]-190, size[1]-42), fill=rgba(PALETTE["border_subtle"],220))
    d.rectangle((24, size[1]-48, size[0]//2, size[1]-42), fill=rgba(color,180))
    return base


def generate_mascot_states() -> None:
    save(aria_character("idle",(64,86)), OUT/"mascot"/"aria-avatar-64.png")
    save(aria_character("idle",(128,172)), OUT/"mascot"/"aria-avatar-128.png")
    for name,state in [
        ("aria-idle.png","idle"),("aria-success.png","success"),("aria-warning.png","warning"),("aria-error.png","error"),
        ("aria-loading-01.png","loading-01"),("aria-loading-02.png","loading-02"),
        ("aria-empty-api-key.png","warning"),("aria-empty-history.png","idle"),("aria-test-model.png","success"),("aria-system-health.png","success")
    ]:
        save(aria_character(state,(288,384)), OUT/"mascot"/name)
    save(mecha_silhouette((960,720)), OUT/"mascot"/"mecha-silhouette.webp")
    save(draw_hud_background((1280,720),variant="control-room"), OUT/"mascot"/"mecha-control-room.webp")
    save(drone("idle"), OUT/"mascot"/"drone-mascot.png")
    save(drone("connected"), OUT/"mascot"/"drone-connected.png")
    save(drone("error"), OUT/"mascot"/"drone-error.png")
    for name in ["empty-api-keys","empty-models","empty-history","empty-proxy","disconnected","server-error","access-denied","test-success","test-failed"]:
        save(compose_state(name), OUT/"states"/f"{name}.webp")
    # Two-frame animated loading core.
    frames=[]
    for phase in range(2):
        frame=Image.new("RGBA",(192,192),rgba(PALETTE["canvas"])); fd=ImageDraw.Draw(frame)
        fd.ellipse((28,28,164,164),outline=rgba(PALETTE["primary"],180),width=6)
        fd.ellipse((55,55,137,137),outline=rgba(PALETTE["cyan"],200),width=5)
        for i in range(8):
            a=(i+phase)*math.pi/4; x=96+int(math.cos(a)*68); y=96+int(math.sin(a)*68)
            fd.rectangle((x-5,y-5,x+5,y+5),fill=rgba(PALETTE["pink"] if i%2 else PALETTE["cyan"]))
        fd.rectangle((80,80,112,112),fill=rgba(PALETTE["primary_hover"]))
        frames.append(frame)
    frames[0].save(OUT/"states"/"loading-core.webp",format="WEBP",save_all=True,append_images=frames[1:],duration=320,loop=0,lossless=True)


def button_bg(kind: str, hover: bool=False) -> Image.Image:
    img=Image.new("RGBA",(192,48),(0,0,0,0)); d=ImageDraw.Draw(img)
    color=PALETTE["error"] if kind=="danger" else PALETTE["primary"] if kind=="primary" else PALETTE["border"]
    if hover: color=PALETTE["error"] if kind=="danger" else PALETTE["primary_hover"] if kind=="primary" else PALETTE["cyan"]
    d.polygon([(6,0),(186,0),(192,6),(192,42),(186,48),(6,48),(0,42),(0,6)],fill=rgba(PALETTE["surface"],235),outline=rgba(color,240))
    d.line((12,4,80,4),fill=rgba(color,180),width=2); d.line((112,44,180,44),fill=rgba(color,120),width=2)
    return img


def generate_buttons() -> None:
    for kind in ("primary","secondary","danger"):
        save(button_bg(kind,False),OUT/"buttons"/f"button-{kind}-bg.png")
        save(button_bg(kind,True),OUT/"buttons"/f"button-{kind}-hover-bg.png")
    save(frame_image("active",48),OUT/"buttons"/"button-icon-frame.png")
    spark=Image.new("RGBA",(24,24),(0,0,0,0)); sd=ImageDraw.Draw(spark); c=rgba(PALETTE["pink"])
    sd.line((12,2,12,22),fill=c,width=2); sd.line((2,12,22,12),fill=c,width=2); sd.line((6,6,18,18),fill=c); sd.line((18,6,6,18),fill=c)
    save(spark,OUT/"buttons"/"button-corner-sparkle.png")


def build_preview() -> None:
    board=Image.new("RGBA",(1600,1000),rgba(PALETTE["canvas"])); d=ImageDraw.Draw(board)
    # Token stripes.
    colors=["canvas","sidebar","surface","surface_active","primary","pink","cyan","success","warning","error"]
    for i,name in enumerate(colors): d.rectangle((40+i*150,32,170+i*150,88),fill=rgba(PALETTE[name]))
    board.alpha_composite(logo_mark(160,True),(50,130))
    board.alpha_composite(aria_character("idle",(240,320)),(250,125))
    board.alpha_composite(mecha_silhouette((400,300)),(490,130))
    board.alpha_composite(drone("connected",160),(900,180))
    # Frames/buttons/icons.
    for i,kind in enumerate(["default","active","danger","modal"]): board.alpha_composite(frame_image(kind,128),(40+i*155,500))
    for i,name in enumerate(["dashboard","api-keys","models","history","system","play","restart","stop"]): board.alpha_composite(draw_icon(name,64,PALETTE["cyan"] if i%2 else PALETTE["primary_hover"]),(40+i*90,700))
    for i,kind in enumerate(["primary","secondary","danger"]): board.alpha_composite(button_bg(kind,False).resize((288,72),Image.Resampling.NEAREST),(40+i*330,820))
    save(board,PREVIEW)


def main() -> None:
    ensure_dirs()
    generate_branding()
    generate_backgrounds()
    generate_frames()
    generate_icons()
    generate_status_metrics()
    generate_mascot_states()
    generate_buttons()
    build_preview()
    files=list(OUT.rglob("*"))
    print(f"Generated {sum(p.is_file() for p in files)} production assets under {OUT}")
    print(f"Preview board: {PREVIEW}")


if __name__ == "__main__":
    main()
