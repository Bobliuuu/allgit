import math, os, sys
from PIL import Image, ImageDraw, ImageFont

W, H, S = 1280, 720, 2          # final size, supersample
FPS, DUR = 24, 13.0
OUT = sys.argv[1]
os.makedirs(OUT, exist_ok=True)

MONO = "/System/Library/Fonts/Menlo.ttc"
SANS = "/System/Library/Fonts/Helvetica.ttc"
def font(path, size, idx=0):
    return ImageFont.truetype(path, int(size * S), index=idx)
F_TITLE = font(SANS, 46, 1)
F_LANE = font(MONO, 26, 1)
F_CAP = font(SANS, 34, 1)
F_SMALL = font(MONO, 20, 0)
F_TAG = font(MONO, 18, 1)

BG = (13, 17, 23)
GRID = (33, 38, 45)
TXT = (230, 237, 243)
MUTED = (125, 133, 144)
ENG = (88, 166, 255)
AST = (63, 185, 80)
ROO = (255, 166, 87)
GOLD = (255, 213, 79)
LANES = [("engine/", ENG, 4), ("assets/", AST, 2), ("root", ROO, 3)]

def ease(t):
    t = max(0.0, min(1.0, t))
    return t * t * (3 - 2 * t)
def seg(t, a, b):
    return ease((t - a) / (b - a))
def mix(c1, c2, k):
    return tuple(int(c1[i] + (c2[i] - c1[i]) * k) for i in range(3))
def sc(v):
    return int(v * S)

LANE_Y = [250, 365, 480]
X0, X1 = 250, 840            # lane span
DOTX = {  # commit x positions per lane
    0: [300, 420, 540, 660],
    1: [300, 480],
    2: [300, 420, 600],
}
HEADX = 780                  # new head after publish

def text(d, xy, s, f, fill, anchor="la"):
    d.text((sc(xy[0]), sc(xy[1])), s, font=f, fill=fill, anchor=anchor)

def dot(d, x, y, r, col, alpha=1.0, ring=False):
    c = mix(BG, col, alpha)
    if ring:
        d.ellipse([sc(x - r), sc(y - r), sc(x + r), sc(y + r)], outline=c, width=sc(3))
    else:
        d.ellipse([sc(x - r), sc(y - r), sc(x + r), sc(y + r)], fill=c)

def line(d, x0, y0, x1, y1, col, w=3, alpha=1.0, dash=False):
    c = mix(BG, col, alpha)
    if not dash:
        d.line([sc(x0), sc(y0), sc(x1), sc(y1)], fill=c, width=sc(w))
    else:
        n = int(abs(y1 - y0) / 12) if x0 == x1 else int(abs(x1 - x0) / 12)
        for i in range(0, n, 2):
            a, b = i / max(n, 1), min((i + 1) / max(n, 1), 1)
            d.line([sc(x0 + (x1 - x0) * a), sc(y0 + (y1 - y0) * a),
                    sc(x0 + (x1 - x0) * b), sc(y0 + (y1 - y0) * b)], fill=c, width=sc(w))

def frame(t):
    img = Image.new("RGB", (W * S, H * S), BG)
    d = ImageDraw.Draw(img)

    # faint grid
    for gx in range(0, W, 64):
        d.line([sc(gx), 0, sc(gx), sc(H)], fill=GRID, width=1)
    for gy in range(0, H, 64):
        d.line([0, sc(gy), sc(W), sc(gy)], fill=GRID, width=1)

    # title
    k = seg(t, 0.0, 0.8)
    text(d, (W / 2, 78), "one repo, many zones", F_TITLE, mix(BG, TXT, k), "mm")
    text(d, (W / 2, 124), "each zone has its own history", F_SMALL, mix(BG, MUTED, k), "mm")

    # scene flags
    sub_k = seg(t, 5.0, 6.2)           # subscribe scene strength
    sub_out = seg(t, 8.4, 9.2)         # fade subscription emphasis out
    sub = sub_k * (1 - sub_out)
    pub_t = t - 9.2                    # publish scene clock

    for i, (name, col, n) in enumerate(LANES):
        y = LANE_Y[i]
        lane_in = seg(t, 0.4 + i * 0.25, 1.1 + i * 0.25)

        # dim lanes that are not subscribed
        dim = 0.0
        if i > 0:
            dim = 0.62 * sub
        base = lane_in * (1 - dim)

        # lane label
        text(d, (60, y), name, F_LANE, mix(BG, col, base), "lm")

        # lane line grows as commits appear
        last = DOTX[i][-1]
        reach = X0 + (last - X0) * seg(t, 1.0 + i * 0.25, 3.6 + i * 0.1)
        line(d, X0, y, reach, y, col, 4, base * 0.9)

        # commits pop in sequence
        for j, x in enumerate(DOTX[i]):
            tj = 1.3 + i * 0.25 + j * 0.45
            kk = seg(t, tj, tj + 0.35)
            if kk > 0:
                r = 13 * (0.4 + 0.6 * kk)
                dot(d, x, y, r, col, base * kk)
                if kk < 1:
                    dot(d, x, y, r + 10 * kk, col, 0.5 * (1 - kk), ring=True)

        # head after publish (ghost then solid)
        if pub_t > 0:
            ghost = seg(pub_t, 0.0, 0.6)
            solid = seg(pub_t, 2.0, 2.5)
            lx = DOTX[i][-1]
            ext = lx + (HEADX - lx) * seg(pub_t, 2.0, 2.7)
            if solid > 0:
                line(d, lx, y, ext, y, col, 4, 0.95)
                dot(d, HEADX, y, 15 * (0.5 + 0.5 * solid), col, solid)
                if solid < 1:
                    dot(d, HEADX, y, 15 + 16 * solid, col, 0.6 * (1 - solid), ring=True)
            else:
                dot(d, HEADX, y, 14, col, 0.55 * ghost, ring=True)

    # "your clone" box (subscribe scene)
    if sub > 0:
        bx0, by0, bx1, by1 = 940, 200, 1220, 380
        c = mix(BG, ENG, 0.9 * sub)
        d.rounded_rectangle([sc(bx0), sc(by0), sc(bx1), sc(by1)], radius=sc(16), outline=c, width=sc(3),
                            fill=mix(BG, (22, 34, 52), sub))
        text(d, ((bx0 + bx1) / 2, by0 + 34), "your clone", F_TAG, mix(BG, TXT, sub), "mm")
        text(d, ((bx0 + bx1) / 2, by0 + 84), "engine/", F_LANE, mix(BG, ENG, sub), "mm")
        text(d, ((bx0 + bx1) / 2, by0 + 128), "downloaded", F_SMALL, mix(BG, MUTED, sub), "mm")
        # flow dots from engine lane to box
        for q in range(5):
            ph = ((t * 0.9) + q / 5.0) % 1.0
            fx = 700 + (bx0 - 700) * ph
            a = math.sin(ph * math.pi) * sub
            dot(d, fx, LANE_Y[0], 7, ENG, a)
        line(d, 700, LANE_Y[0], bx0, LANE_Y[0], ENG, 2, 0.35 * sub)
        # "not downloaded" tags
        for i in (1, 2):
            text(d, (700, LANE_Y[i]), "not downloaded", F_TAG, mix(BG, MUTED, sub), "lm")
            line(d, X1 + 25, LANE_Y[i], bx0 - 30, LANE_Y[i], MUTED, 2, 0.35 * sub, dash=False) if False else None

    # atomic publish: vertical line sweeping across lanes
    if pub_t > 0.6:
        sweep = seg(pub_t, 0.6, 2.0)
        yA, yB = LANE_Y[0] - 56, LANE_Y[2] + 56
        yy = yA + (yB - yA) * sweep
        flash = 1.0 - seg(pub_t, 2.0, 3.2)
        a = (0.35 + 0.65 * flash) * (1 - seg(pub_t, 3.2, 3.8) * 0.7)
        line(d, HEADX, yA, HEADX, yy, GOLD, 4, a)
        dot(d, HEADX, yy, 8, GOLD, a)
        if pub_t > 2.0:
            # bracket joining all heads
            line(d, HEADX, yA, HEADX, yB, GOLD, 3, 0.8 * (1 - seg(pub_t, 3.2, 3.8) * 0.6), dash=True)

    # captions
    caps = [
        (1.0, 4.6, "one repo, three zones", TXT),
        (4.9, 8.6, "you subscribe to what you need", ENG),
        (9.2, 99.0, "one atomic publish: all heads move, or none", GOLD),
    ]
    for a, b, s, col in caps:
        k = seg(t, a, a + 0.5) * (1 - seg(t, b - 0.4, b))
        if k > 0:
            text(d, (W / 2, 610), s, F_CAP, mix(BG, col, k), "mm")
    # sub-caption during subscribe
    k = seg(t, 5.6, 6.2) * (1 - seg(t, 8.0, 8.5))
    if k > 0:
        text(d, (W / 2, 652), "engine only? assets are never downloaded", F_SMALL, mix(BG, MUTED, k), "mm")
    # footer
    kf = seg(t, 11.8, 12.6)
    if kf > 0:
        text(d, (W / 2, 690), "vcs  ·  github.com/Bobliuuu/allgit", F_SMALL, mix(BG, MUTED, kf), "mm")

    return img.resize((W, H), Image.LANCZOS)

n = int(FPS * DUR)
for i in range(n):
    frame(i / FPS).save(f"{OUT}/f{i:04d}.png")
print("frames", n)
