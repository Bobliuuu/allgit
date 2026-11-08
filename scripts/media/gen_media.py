#!/usr/bin/env python3
"""Render docs/media/demo.gif and docs/media/explainer.mp4 from docs/media/transcript.txt.
usage: gen_media.py [gif|video|all]   (needs Pillow + ffmpeg; white backgrounds only)"""
import math, os, subprocess, sys
from PIL import Image, ImageDraw, ImageFont

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
MEDIA = os.path.join(ROOT, "docs", "media")
TRANSCRIPT = os.path.join(MEDIA, "transcript.txt")

def font(kind, size):
    cands = {
        "sans": [("/System/Library/Fonts/HelveticaNeue.ttc", 0), ("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", 0)],
        "bold": [("/System/Library/Fonts/HelveticaNeue.ttc", 1), ("/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf", 0)],
        "mono": [("/System/Library/Fonts/Menlo.ttc", 0), ("/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf", 0)],
        "monob": [("/System/Library/Fonts/Menlo.ttc", 1), ("/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf", 0)],
    }[kind]
    for p, i in cands:
        if os.path.exists(p):
            return ImageFont.truetype(p, size, index=i)
    return ImageFont.load_default()

WHITE = (255, 255, 255); INK = (31, 35, 40); MUTED = (89, 99, 110); LINE = (208, 215, 222); PANEL = (246, 248, 250)
BLUE = (9, 105, 218); BLUE_BG = (221, 244, 255); GREEN = (26, 127, 55); GREEN_BG = (218, 251, 225)
ORANGE = (188, 76, 0); ORANGE_BG = (255, 241, 229); PURPLE = (130, 80, 223); PURPLE_BG = (251, 239, 255)
YELLOW_BG = (255, 248, 197); YELLOW = (191, 135, 0); RED = (207, 34, 46); RED_BG = (255, 235, 233)

# ---------------------------------------------------------------- transcript
def load_transcript():
    cmds = []
    for line in open(TRANSCRIPT):
        line = line.rstrip("\n")
        if line.startswith("$ "):
            cmds.append([line[2:], []])
        else:
            cmds[-1][1].append(line)
    return cmds

# a terminal "session" is a list of events (kind, text): ('cmd',txt) typed, ('out',txt) shown at once
def terminal_lines_state(cmds, n_cmds, typed_chars=None):
    """Lines visible after n_cmds fully finished commands, plus optional partially typed next command."""
    lines = []
    for c, out in cmds[:n_cmds]:
        lines.append(("cmd", c)); lines += [("out", o) for o in out]
    if typed_chars is not None and n_cmds < len(cmds):
        lines.append(("cmd", cmds[n_cmds][0][:typed_chars]))
    return lines

def draw_terminal(img, box, lines, fs, cursor=True, highlight=("NOT deleted", "NOT DELETED", "published atomically")):
    d = ImageDraw.Draw(img)
    x0, y0, x1, y1 = box
    d.rounded_rectangle(box, 12, fill=PANEL, outline=LINE, width=2)
    d.rounded_rectangle((x0, y0, x1, y0 + 34), 12, fill=(234, 238, 242), outline=LINE, width=2)
    d.rectangle((x0 + 2, y0 + 22, x1 - 2, y0 + 34), fill=(234, 238, 242))
    for i, c in enumerate(((255, 95, 86), (255, 189, 46), (39, 201, 63))):
        d.ellipse((x0 + 16 + i * 22, y0 + 11, x0 + 28 + i * 22, y0 + 23), fill=c)
    d.text(((x0 + x1) // 2, y0 + 17), "vcs quickstart (real run)", font=font("sans", 14), fill=MUTED, anchor="mm")
    fm, fb = font("mono", fs), font("monob", fs)
    lh = int(fs * 1.45); pad = 18
    top = y0 + 34 + 12
    rows = (y1 - 12 - top) // lh
    vis = lines[-rows:]
    cw = fm.getlength("M")
    for i, (kind, t) in enumerate(vis):
        y = top + i * lh
        if kind == "cmd":
            d.text((x0 + pad, y), "$", font=fb, fill=GREEN)
            d.text((x0 + pad + cw * 2, y), t, font=fm, fill=INK)
        else:
            hl = any(h in t for h in highlight)
            if hl:
                d.rectangle((x0 + 8, y - 2, x1 - 8, y + lh - 4), fill=YELLOW_BG)
            d.text((x0 + pad, y), t, font=fb if hl else fm, fill=INK if hl else MUTED)
    if cursor and vis:
        kind, t = vis[-1]
        if kind == "cmd":
            cx = x0 + pad + cw * (2 + len(t)); y = top + (len(vis) - 1) * lh
            d.rectangle((cx + 2, y + 2, cx + 2 + int(cw * .6), y + lh - 6), fill=INK)

def terminal_timeline(cmds, fps, speed=1.0):
    """Yields (lines, cursor_on) per frame at the given fps."""
    frames = []
    for n, (c, out) in enumerate(cmds):
        type_frames = max(4, int(len(c) * 0.035 * fps / speed))
        for k in range(type_frames):
            frames.append((terminal_lines_state(cmds, n, int(len(c) * (k + 1) / type_frames)), True))
        for _ in range(max(2, int(0.15 * fps / speed))):
            frames.append((terminal_lines_state(cmds, n, len(c)), True))
        hold = (0.35 if out else 0.2) + 0.12 * len(out)
        for _ in range(max(2, int(hold * fps / speed))):
            frames.append((terminal_lines_state(cmds, n + 1), False))
    frames += [(terminal_lines_state(cmds, len(cmds)), True)] * int(1.2 * fps)
    return frames

# ---------------------------------------------------------------- GIF
def make_gif():
    cmds = load_transcript()
    W, H = 900, 560
    fps = 12
    frames = terminal_timeline(cmds, fps, speed=1.0)
    imgs = []
    prev = None
    for lines, cur in frames:
        im = Image.new("RGB", (W, H), WHITE)
        draw_terminal(im, (20, 20, W - 20, H - 20), lines, 14, cursor=cur)
        imgs.append(im)
    # drop duplicate neighbours, accumulating duration
    out, durs = [], []
    for im in imgs:
        b = im.tobytes()
        if prev == b:
            durs[-1] += 1000 // fps
        else:
            out.append(im.quantize(colors=64, method=Image.Quantize.MEDIANCUT, dither=Image.Dither.NONE)); durs.append(1000 // fps)
        prev = b
    durs[-1] += 2500
    path = os.path.join(MEDIA, "demo.gif")
    out[0].save(path, save_all=True, append_images=out[1:], duration=durs, loop=0, optimize=True, disposal=1)
    print("gif", path, os.path.getsize(path), "bytes", len(out), "frames")

# ---------------------------------------------------------------- video
VW, VH, FPS = 1280, 720, 30
def clamp(x): return max(0.0, min(1.0, x))
def ease(x):
    x = clamp(x); return x * x * (3 - 2 * x)
def seg(t, a, b): return ease((t - a) / (b - a))

def lerp(a, b, k): return a + (b - a) * k
def mix(c, k, base=WHITE): return tuple(int(lerp(base[i], c[i], k)) for i in range(3))

def title(d, text, sub=None, k=1.0):
    d.text((64, 48), text, font=font("bold", 46), fill=mix(INK, k))
    if sub: d.text((64, 108), sub, font=font("sans", 26), fill=mix(MUTED, k))

def caption(d, text, k=1.0):
    d.text((VW // 2, VH - 44), text, font=font("sans", 28), fill=mix(INK, k), anchor="mm")

def box(d, xy, fill, outline, label=None, fs=24, k=1.0, bold=True, tcol=None):
    d.rounded_rectangle(xy, 12, fill=mix(fill, k), outline=mix(outline, k), width=3)
    if label:
        x0, y0, x1, y1 = xy
        d.text(((x0 + x1) // 2, (y0 + y1) // 2), label, font=font("bold" if bold else "sans", fs), fill=mix(tcol or outline, k), anchor="mm")

def arrow(d, p0, p1, col, k=1.0, w=4):
    c = mix(col, k)
    d.line([p0, p1], fill=c, width=w)
    ang = math.atan2(p1[1] - p0[1], p1[0] - p0[0]); L = 14
    for s in (-0.45, 0.45):
        d.line([p1, (p1[0] - L * math.cos(ang + s), p1[1] - L * math.sin(ang + s))], fill=c, width=w)

def dot_chain(d, x, y, n, col, k=1.0, step=46, r=11, reveal=1.0):
    m = int(n * reveal)
    for i in range(m):
        if i: d.line([(x + (i - 1) * step, y), (x + i * step, y)], fill=mix(col, k), width=3)
    for i in range(m):
        d.ellipse((x + i * step - r, y - r, x + i * step + r, y + r), fill=mix(col, k))

def scene_problem(t):
    im = Image.new("RGB", (VW, VH), WHITE); d = ImageDraw.Draw(im)
    title(d, "One big monorepo", "Code and art share a single history", seg(t, 0, .6))
    # repo cylinder-ish box with mixed commits
    k = seg(t, .4, 1.2)
    box(d, (400, 190, 880, 450), PANEL, LINE, None, k=k)
    d.text((640, 215), "monorepo history", font=font("bold", 26), fill=mix(INK, k), anchor="mm")
    for i in range(10):
        col = BLUE if i % 3 != 2 else GREEN
        cx = 450 + i * 42
        if seg(t, 1 + i * .12, 1.2 + i * .12) > 0:
            d.ellipse((cx - 13, 300 - 13, cx + 13, 300 + 13), fill=col)
            if i: d.line([(cx - 42 + 13, 300), (cx - 13, 300)], fill=col, width=3)
    d.text((470, 350), "code commits", font=font("sans", 22), fill=BLUE)
    d.text((680, 350), "art commits", font=font("sans", 22), fill=GREEN)
    d.text((640, 410), "every commit, every file, one timeline", font=font("sans", 22), fill=MUTED, anchor="mm")
    # three devs each downloading everything
    for i, x in enumerate((180, 640, 1100)):
        a = seg(t, 3.2 + i * .35, 4.4 + i * .35)
        if a > 0:
            arrow(d, (640 + (x - 640) * .55, 455), (x, lerp(455, 560, a)), RED, a)
            box(d, (x - 110, 560, x + 110, 630), RED_BG, RED, "full clone", 24, a)
    caption(d, "Everyone downloads everything", seg(t, 4.6, 5.4))
    return im

def scene_zones(t):
    im = Image.new("RGB", (VW, VH), WHITE); d = ImageDraw.Draw(im)
    title(d, "Zones", "Independent histories in one repository", seg(t, 0, .6))
    lanes = [("root", ORANGE, ORANGE_BG, 5), ("engine", BLUE, BLUE_BG, 7), ("assets", GREEN, GREEN_BG, 4)]
    for i, (nm, c, bg, n) in enumerate(lanes):
        y = 190 + i * 100
        a = seg(t, .6 + i * .5, 1.4 + i * .5)
        box(d, (64, y, 1216, y + 80), bg, c, None, k=a)
        d.text((90, y + 40), nm, font=font("bold", 30), fill=mix(c, a), anchor="lm")
        dot_chain(d, 300, y + 40, n, c, a, step=70, r=14, reveal=seg(t, 1.4 + i * .5, 3 + i * .5))
        d.text((1190, y + 40), "own heads + history", font=font("sans", 22), fill=mix(MUTED, a), anchor="rm")
    k = seg(t, 4, 5)
    for x in (200, 640, 1080):
        arrow(d, (x, 490), (x, 520), MUTED, k, 3)
    box(d, (64, 525, 1216, 600), PANEL, LINE, None, k=k)
    d.text((640, 562), "one deduplicated object graph", font=font("bold", 30), fill=mix(INK, k), anchor="mm")
    caption(d, "Identical content is stored once, across every zone", seg(t, 5.2, 6))
    return im

def scene_sparse(t):
    im = Image.new("RGB", (VW, VH), WHITE); d = ImageDraw.Draw(im)
    title(d, "Sparse clone", "Download only the zones you need", seg(t, 0, .6))
    d.text((64, 190), "remote", font=font("bold", 26), fill=INK)
    zs = [("root", ORANGE, ORANGE_BG), ("engine", BLUE, BLUE_BG), ("assets", GREEN, GREEN_BG)]
    for i, (nm, c, bg) in enumerate(zs):
        y = 235 + i * 105
        box(d, (64, y, 420, y + 85), bg, c, nm, 30, k=seg(t, .4 + i * .3, 1.2 + i * .3))
    cmdk = seg(t, 2.2, 3)
    d.rounded_rectangle((470, 190, 1216, 250), 10, fill=mix(PANEL, cmdk), outline=mix(LINE, cmdk), width=2)
    d.text((490, 220), "$ vcs clone shared.vcs work --zone engine", font=font("mono", 25), fill=mix(INK, cmdk), anchor="lm")
    d.text((850, 285), "your workspace", font=font("bold", 26), fill=mix(INK, cmdk), anchor="mm")
    a = seg(t, 3.4, 4.4)
    arrow(d, (420, 320), (lerp(420, 640, a), 320), BLUE, a)
    box(d, (650, 300, 1050, 380), BLUE_BG, BLUE, "engine  downloaded", 26, k=a)
    for i, nm in enumerate(("root", "assets")):
        b = seg(t, 5, 6)
        y = 405 + i * 95
        d.rounded_rectangle((650, y, 1050, y + 75), 12, fill=mix(PANEL, b), outline=mix(LINE, b), width=3)
        d.text((850, y + 37), f"{nm}  not subscribed", font=font("sans", 26), fill=mix(MUTED, b), anchor="mm")
    caption(d, "Skipped zones are not downloaded, and not deleted", seg(t, 6, 6.8))
    return im

def scene_publish(t):
    im = Image.new("RGB", (VW, VH), WHITE); d = ImageDraw.Draw(im)
    title(d, "Atomic cross-zone publish", "Objects first, then one compare-and-swap", seg(t, 0, .6))
    box(d, (64, 230, 330, 430), BLUE_BG, BLUE, None, k=seg(t, .4, 1))
    d.text((197, 265), "local repo", font=font("bold", 28), fill=BLUE, anchor="mm")
    for i, (nm, c) in enumerate((("root", ORANGE), ("engine", BLUE), ("assets", GREEN))):
        d.rounded_rectangle((90, 300 + i * 40, 300, 330 + i * 40), 6, fill=mix(c, .25), outline=c, width=2)
        d.text((195, 315 + i * 40), nm, font=font("sans", 20), fill=INK, anchor="mm")
    box(d, (850, 200, 1216, 480), PANEL, LINE, None, k=seg(t, .4, 1))
    d.text((1033, 232), "S3 / directory remote", font=font("bold", 26), fill=INK, anchor="mm")
    d.rounded_rectangle((875, 265, 1190, 345), 8, fill=WHITE, outline=LINE, width=2)
    d.text((1033, 305), "objects", font=font("sans", 26), fill=MUTED, anchor="mm")
    # step 1
    a = seg(t, 1.5, 3.5)
    if a > 0:
        d.text((590, 262), "1  upload objects", font=font("bold", 26), fill=mix(BLUE, seg(t, 1.5, 2)), anchor="mm")
        for j in range(3):
            x = lerp(340, 860, clamp(a * 1.3 - j * .12)) 
            d.rounded_rectangle((x, 290 + j * 14, x + 40, 304 + j * 14), 4, fill=mix((BLUE, GREEN, ORANGE)[j], 1))
    b = seg(t, 4.2, 5.2)
    d.rounded_rectangle((875, 365, 1190, 455), 8, fill=mix(YELLOW_BG, b), outline=mix(YELLOW, b), width=3)
    d.text((1033, 395), "manifest", font=font("bold", 26), fill=mix(INK, b), anchor="mm")
    gen = "generation 0" if t < 6.4 else "generation 1"
    d.text((1033, 432), gen, font=font("mono", 22), fill=mix(YELLOW, b), anchor="mm")
    if b > 0:
        d.text((590, 400), "2  swap manifest, once", font=font("bold", 26), fill=mix(YELLOW, b), anchor="mm")
        c = seg(t, 5.2, 6.4)
        arrow(d, (340, 430), (lerp(340, 860, c), 430), YELLOW, b)
    if t > 6.4:
        k = seg(t, 6.4, 7.2)
        d.text((640, 560), "published atomically (remote generation 1)", font=font("monob", 30), fill=mix(GREEN, k), anchor="mm")
    caption(d, "Readers see all zones advance together, or none", seg(t, 7.4, 8.2))
    return im

TERM_CMDS = None
def scene_terminal(t, dur):
    global TERM_CMDS
    if TERM_CMDS is None:
        TERM_CMDS = terminal_timeline(load_transcript(), FPS, speed=1.4)
    im = Image.new("RGB", (VW, VH), WHITE); d = ImageDraw.Draw(im)
    d.text((64, 26), "Real terminal run", font=font("bold", 38), fill=INK)
    i = min(int(t * FPS), len(TERM_CMDS) - 1)
    lines, cur = TERM_CMDS[i]
    draw_terminal(im, (64, 84, VW - 64, VH - 24), lines, 20, cursor=cur)
    return im

def scene_close(t):
    im = Image.new("RGB", (VW, VH), WHITE); d = ImageDraw.Draw(im)
    k = seg(t, 0, 1)
    d.text((640, 300), "vcs", font=font("bold", 150), fill=mix(INK, k), anchor="mm")
    k2 = seg(t, .8, 1.8)
    d.text((640, 430), "github.com/Bobliuuu/allgit", font=font("sans", 44), fill=mix(BLUE, k2), anchor="mm")
    d.text((640, 500), "zones, sparse clones, atomic publishes", font=font("sans", 28), fill=mix(MUTED, k2), anchor="mm")
    return im

def scene_poster():
    # first/best frame: title + three-zone motif
    im = Image.new("RGB", (VW, VH), WHITE); d = ImageDraw.Draw(im)
    d.text((640, 200), "vcs", font=font("bold", 130), fill=INK, anchor="mm")
    d.text((640, 300), "Zones, sparse clones, atomic publishes", font=font("sans", 40), fill=MUTED, anchor="mm")
    for i, (nm, c, bg) in enumerate((("root", ORANGE, ORANGE_BG), ("engine", BLUE, BLUE_BG), ("assets", GREEN, GREEN_BG))):
        x = 240 + i * 280
        box(d, (x, 380, x + 240, 470), bg, c, nm, 32)
        dot_chain(d, x + 40, 520, 4, c, step=55, r=11)
    d.text((640, 640), "github.com/Bobliuuu/allgit", font=font("sans", 32), fill=BLUE, anchor="mm")
    return im

def make_video():
    term_dur = len(terminal_timeline(load_transcript(), FPS, speed=1.4)) / FPS
    SL = 0.75  # slow-down: scene animations run at 75% speed so captions stay readable
    sl = lambda fn: (lambda t: fn(t * SL))
    scenes = [(sl(scene_problem), 7.5 / SL), (sl(scene_zones), 7.5 / SL), (sl(scene_sparse), 8.5 / SL), (sl(scene_publish), 9.5 / SL),
              (lambda t: scene_terminal(t, term_dur), term_dur), (scene_close, 4.0)]
    total = sum(s for _, s in scenes)
    print("video duration", total)
    out = os.path.join(MEDIA, "explainer.mp4")
    p = subprocess.Popen(["ffmpeg", "-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", f"{VW}x{VH}",
                          "-r", str(FPS), "-i", "-", "-vf", "scale=in_range=full:out_range=tv:out_color_matrix=bt709:flags=accurate_rnd+full_chroma_int",
                          "-c:v", "libx264", "-preset", "slow", "-crf", "18", "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709", "-color_range", "tv",
                          "-pix_fmt", "yuv420p", "-movflags", "+faststart", out], stdin=subprocess.PIPE)
    fade = int(.35 * FPS)
    for fn, dur in scenes:
        n = int(dur * FPS)
        for f in range(n):
            im = fn(f / FPS)
            # white-through fades between scenes
            if f < fade: im = Image.blend(Image.new("RGB", im.size, WHITE), im, f / fade)
            if f > n - fade: im = Image.blend(Image.new("RGB", im.size, WHITE), im, (n - f) / fade)
            p.stdin.write(im.tobytes())
    p.stdin.close(); p.wait()
    scene_poster().save(os.path.join(MEDIA, "explainer-poster.png"))
    print("wrote", out, os.path.getsize(out))

if __name__ == "__main__":
    what = sys.argv[1] if len(sys.argv) > 1 else "all"
    if what in ("gif", "all"): make_gif()
    if what in ("video", "all"): make_video()
