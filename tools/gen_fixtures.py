#!/usr/bin/env python3
"""Generate ASCII DXF test fixtures for opendxfviewer.

Hand-written DXF text (rather than a library) so we control exactly which group
codes appear, including the awkward cases real files contain.
"""
import math, os, pathlib

OUT = pathlib.Path(__file__).resolve().parent.parent / "tests" / "fixtures"


class Dxf:
    def __init__(self, version="AC1021", codepage="ANSI_1252"):
        self.version = version           # AC1021 = R2007, the release that moved DXF to UTF-8
        self.codepage = codepage
        self.p = []
        self.layers = []
        self.blocks = []   # (name, base, [entity chunks])
        self.ents = []

    def g(self, code, val):
        self.p.append(f"{code}\n{val}")

    def layer(self, name, aci=7, ltype="CONTINUOUS"):
        self.layers.append((name, aci, ltype))
        return name

    def block(self, name, base, ents):
        self.blocks.append((name, base, ents))
        return name

    def add(self, chunk):
        self.ents.append(chunk)

    def render(self):
        o = []
        a = o.append

        def g(c, v):
            a(f"{c}\n{v}")

        # ---- HEADER
        g(0, "SECTION"); g(2, "HEADER")
        g(9, "$ACADVER"); g(1, self.version)
        g(9, "$DWGCODEPAGE"); g(3, self.codepage)
        g(9, "$INSUNITS"); g(70, 4)          # millimeters
        g(9, "$EXTMIN"); g(10, -1000.0); g(20, -1000.0); g(30, 0.0)
        g(9, "$EXTMAX"); g(10, 1000.0); g(20, 1000.0); g(30, 0.0)
        g(9, "$LTSCALE"); g(40, 1.0)
        g(0, "ENDSEC")

        # ---- TABLES (layers + linetypes)
        g(0, "SECTION"); g(2, "TABLES")
        g(0, "TABLE"); g(2, "LTYPE"); g(70, 2)
        for nm, pat in [("CONTINUOUS", []), ("DASHED", [12.7, -6.35]), ("CENTER", [31.75, -6.35, 6.35, -6.35])]:
            g(0, "LTYPE"); g(2, nm); g(70, 0); g(3, nm.title())
            g(72, 65); g(73, len(pat)); g(40, sum(abs(x) for x in pat))
            for d in pat:
                g(49, d); g(74, 0)
        g(0, "ENDTAB")
        g(0, "TABLE"); g(2, "LAYER"); g(70, len(self.layers))
        for nm, aci, lt in self.layers:
            g(0, "LAYER"); g(2, nm); g(70, 0); g(62, aci); g(6, lt); g(370, 25)
        g(0, "ENDTAB")
        g(0, "ENDSEC")

        # ---- BLOCKS
        g(0, "SECTION"); g(2, "BLOCKS")
        for nm, base, ents in self.blocks:
            g(0, "BLOCK"); g(8, "0"); g(2, nm); g(70, 0)
            g(10, base[0]); g(20, base[1]); g(30, 0.0); g(3, nm)
            for e in ents:
                a(e)
            g(0, "ENDBLK"); g(8, "0")
        g(0, "ENDSEC")

        # ---- ENTITIES
        g(0, "SECTION"); g(2, "ENTITIES")
        for e in self.ents:
            a(e)
        g(0, "ENDSEC")
        g(0, "EOF")
        return "\n".join(o) + "\n"


def chunk(*pairs):
    return "\n".join(f"{c}\n{v}" for c, v in pairs)


def line(x1, y1, x2, y2, layer="0", color=None, ltype=None, z1=0.0, z2=0.0):
    p = [(0, "LINE"), (8, layer)]
    if color is not None: p.append((62, color))
    if ltype is not None: p.append((6, ltype))
    p += [(10, x1), (20, y1), (30, z1), (11, x2), (21, y2), (31, z2)]
    return chunk(*p)


def circle(cx, cy, r, layer="0", color=None, normal=None):
    p = [(0, "CIRCLE"), (8, layer)]
    if color is not None: p.append((62, color))
    p += [(10, cx), (20, cy), (30, 0.0), (40, r)]
    if normal: p += [(210, normal[0]), (220, normal[1]), (230, normal[2])]
    return chunk(*p)


def arc(cx, cy, r, a0, a1, layer="0", color=None, normal=None):
    p = [(0, "ARC"), (8, layer)]
    if color is not None: p.append((62, color))
    p += [(10, cx), (20, cy), (30, 0.0), (40, r), (50, a0), (51, a1)]
    if normal: p += [(210, normal[0]), (220, normal[1]), (230, normal[2])]
    return chunk(*p)


def point(x, y, layer="0", color=None):
    p = [(0, "POINT"), (8, layer)]
    if color is not None: p.append((62, color))
    p += [(10, x), (20, y), (30, 0.0)]
    return chunk(*p)


def lwpoly(pts, closed=False, layer="0", color=None, width=None, elev=0.0, normal=None):
    """pts: list of (x, y) or (x, y, bulge)."""
    p = [(0, "LWPOLYLINE"), (8, layer), (100, "AcDbEntity"), (100, "AcDbPolyline")]
    p = [(0, "LWPOLYLINE"), (8, layer)]
    if color is not None: p.append((62, color))
    p += [(90, len(pts)), (70, 1 if closed else 0)]
    if width is not None: p.append((43, width))
    if elev: p.append((38, elev))
    for v in pts:
        p += [(10, v[0]), (20, v[1])]
        if len(v) > 2 and v[2] != 0.0:
            p.append((42, v[2]))
    if normal: p += [(210, normal[0]), (220, normal[1]), (230, normal[2])]
    return chunk(*p)


def polyline(pts, closed=False, layer="0", color=None):
    """Old-style POLYLINE/VERTEX/SEQEND. pts: (x, y[, bulge])."""
    p = [(0, "POLYLINE"), (8, layer)]
    if color is not None: p.append((62, color))
    p += [(66, 1), (10, 0.0), (20, 0.0), (30, 0.0), (70, 1 if closed else 0)]
    out = [chunk(*p)]
    for v in pts:
        vp = [(0, "VERTEX"), (8, layer), (10, v[0]), (20, v[1]), (30, 0.0), (70, 0)]
        if len(v) > 2 and v[2] != 0.0:
            vp.append((42, v[2]))
        out.append(chunk(*vp))
    out.append(chunk((0, "SEQEND"), (8, layer)))
    return "\n".join(out)


def ellipse(cx, cy, mx, my, ratio, t0=0.0, t1=2 * math.pi, layer="0", color=None):
    p = [(0, "ELLIPSE"), (8, layer)]
    if color is not None: p.append((62, color))
    p += [(10, cx), (20, cy), (30, 0.0), (11, mx), (21, my), (31, 0.0),
          (210, 0.0), (220, 0.0), (230, 1.0), (40, ratio), (41, t0), (42, t1)]
    return chunk(*p)


def spline(ctrl, knots, degree=3, closed=False, layer="0", color=None, weights=None, fit=None):
    flags = 8 if closed else 8
    flags = (1 if closed else 0) | 8  # 8 = planar
    p = [(0, "SPLINE"), (8, layer)]
    if color is not None: p.append((62, color))
    p += [(210, 0.0), (220, 0.0), (230, 1.0),
          (70, flags), (71, degree), (72, len(knots)), (73, len(ctrl)), (74, len(fit or []))]
    for k in knots:
        p.append((40, k))
    if weights:
        for w in weights:
            p.append((41, w))
    for c in ctrl:
        p += [(10, c[0]), (20, c[1]), (30, 0.0)]
    for f in (fit or []):
        p += [(11, f[0]), (21, f[1]), (31, 0.0)]
    return chunk(*p)


def text(x, y, h, s, rot=0.0, layer="0", color=None, halign=0, valign=0, x2=None, y2=None):
    p = [(0, "TEXT"), (8, layer)]
    if color is not None: p.append((62, color))
    p += [(10, x), (20, y), (30, 0.0), (40, h), (1, s), (50, rot), (72, halign)]
    if x2 is not None:
        p += [(11, x2), (21, y2), (31, 0.0)]
    p.append((73, valign))
    return chunk(*p)


def mtext(x, y, h, s, width=0.0, attach=1, rot=0.0, layer="0", color=None):
    p = [(0, "MTEXT"), (8, layer)]
    if color is not None: p.append((62, color))
    p += [(10, x), (20, y), (30, 0.0), (40, h), (41, width), (71, attach), (1, s), (50, rot)]
    return chunk(*p)


def insert(name, x, y, sx=1.0, sy=1.0, rot=0.0, cols=1, rows=1, cspace=0.0, rspace=0.0,
           layer="0", color=None, attribs=None):
    """attribs: list of (tag, value, x, y, height) drawn after the INSERT, as AutoCAD writes them."""
    p = [(0, "INSERT"), (8, layer)]
    if color is not None: p.append((62, color))
    if attribs: p.append((66, 1))          # "attributes follow"
    p += [(2, name), (10, x), (20, y), (30, 0.0), (41, sx), (42, sy), (43, 1.0), (50, rot)]
    if cols > 1 or rows > 1:
        p += [(70, cols), (71, rows), (44, cspace), (45, rspace)]
    out = [chunk(*p)]
    for (tag, value, ax, ay, ah) in (attribs or []):
        out.append(chunk((0, "ATTRIB"), (8, layer), (10, ax), (20, ay), (30, 0.0),
                         (40, ah), (1, value), (2, tag), (70, 0), (50, 0.0)))
    if attribs:
        out.append(chunk((0, "SEQEND"), (8, layer)))
    return "\n".join(out)


def ray(x, y, dx, dy, layer="0", color=None):
    p = [(0, "RAY"), (8, layer)]
    if color is not None: p.append((62, color))
    p += [(10, x), (20, y), (30, 0.0), (11, dx), (21, dy), (31, 0.0)]
    return chunk(*p)


def xline(x, y, dx, dy, layer="0", color=None):
    p = [(0, "XLINE"), (8, layer)]
    if color is not None: p.append((62, color))
    p += [(10, x), (20, y), (30, 0.0), (11, dx), (21, dy), (31, 0.0)]
    return chunk(*p)


def solid(pts, layer="0", color=None):
    p = [(0, "SOLID"), (8, layer)]
    if color is not None: p.append((62, color))
    for i, v in enumerate(pts):
        p += [(10 + i, v[0]), (20 + i, v[1]), (30 + i, 0.0)]
    return chunk(*p)


def face3d(pts, layer="0", color=None):
    p = [(0, "3DFACE"), (8, layer)]
    if color is not None: p.append((62, color))
    for i, v in enumerate(pts):
        p += [(10 + i, v[0]), (20 + i, v[1]), (30 + i, v[2] if len(v) > 2 else 0.0)]
    return chunk(*p)


# ---------------------------------------------------------------- fixtures
def f_basic():
    d = Dxf()
    d.layer("0", 7); d.layer("WALLS", 1); d.layer("DIMS", 3, "DASHED")
    d.layer("HIDDEN", 5, "CENTER")
    # frame
    d.add(lwpoly([(0, 0), (200, 0), (200, 150), (0, 150)], closed=True, layer="WALLS"))
    for i in range(10):
        d.add(line(10 + i * 5, 10, 10 + i * 5, 140, layer="DIMS"))
    d.add(circle(100, 75, 40, layer="0", color=2))
    d.add(circle(100, 75, 30, layer="HIDDEN"))
    d.add(arc(100, 75, 50, 0, 90, layer="0", color=5))
    d.add(arc(100, 75, 55, 270, 45, layer="0", color=6))     # wraps 360
    d.add(arc(100, 75, 60, -30, 30, layer="0", color=1))     # negative start
    for i in range(20):
        a = i * math.pi / 10
        d.add(point(100 + 70 * math.cos(a), 75 + 70 * math.sin(a), color=4))
    d.add(line(0, 0, 200, 150, color=256))    # ByLayer
    d.add(line(0, 150, 200, 0, color=0))      # ByBlock at top level
    return d


def f_polylines():
    d = Dxf()
    d.layer("0", 7); d.layer("CURVY", 30)
    # bulge sanity: square with rounded corners
    b = math.tan(math.radians(90) / 4)   # quarter-circle bulge = 0.41421
    d.add(lwpoly([(0, 10, 0), (0, 40, b), (10, 50, 0), (40, 50, b),
                  (50, 40, 0), (50, 10, b), (40, 0, 0), (10, 0, b)],
                 closed=True, layer="CURVY"))
    # semicircle bulges, both signs
    d.add(lwpoly([(70, 0, 1.0), (110, 0, 1.0)], closed=True))          # full circle from 2 pts
    d.add(lwpoly([(70, 30, -1.0), (110, 30, -1.0)], closed=True))      # cw
    d.add(lwpoly([(70, 60, 0.5), (110, 60, -0.5), (110, 80, 0)], closed=False, color=2))
    # heavy polyline (constant width) -> still rendered as centreline
    d.add(lwpoly([(0, 70), (50, 70), (50, 100)], width=2.0, color=3))
    # old-style POLYLINE with bulges
    d.add(polyline([(130, 0, 0), (180, 0, 0.5), (180, 50, 0), (130, 50, 0.5)],
                   closed=True, layer="CURVY", color=6))
    # degenerate: single vertex, duplicate points
    d.add(lwpoly([(200, 200)], closed=False))
    d.add(lwpoly([(210, 200), (210, 200), (210, 200)], closed=False))
    return d


def f_curves():
    d = Dxf()
    d.layer("0", 7); d.layer("SPL", 5)
    # full ellipse, rotated major axis
    d.add(ellipse(0, 0, 50, 0, 0.5))
    d.add(ellipse(0, 0, 35.36, 35.36, 0.4, color=2))                 # 45deg major axis
    # elliptical arc (parameter space, not true anomaly)
    d.add(ellipse(120, 0, 40, 0, 0.35, 0.0, math.pi, color=3))
    d.add(ellipse(120, 60, 40, 0, 0.35, math.pi * 1.5, math.pi * 0.5, color=4))  # wraps
    # clamped cubic bspline, 5 control points
    ctrl = [(0, 100), (20, 160), (60, 60), (100, 160), (140, 100)]
    knots = [0, 0, 0, 0, 0.5, 1, 1, 1, 1]
    d.add(spline(ctrl, knots, 3, layer="SPL"))
    # rational (weighted) quadratic = exact circular arc
    w = math.sqrt(2) / 2
    d.add(spline([(200, 0), (240, 0), (240, 40)], [0, 0, 0, 1, 1, 1], 2,
                 weights=[1.0, w, 1.0], color=1))
    # periodic/closed spline
    cc = [(200, 100), (240, 100), (240, 140), (200, 140)]
    d.add(spline(cc, [-1, 0, 1, 2, 3, 4, 5, 6], 2, closed=True, color=6))
    # degree-1 spline == polyline
    d.add(spline([(0, 200), (50, 230), (100, 200)], [0, 0, 1, 2, 2], 1, color=4))
    return d


def f_blocks():
    d = Dxf()
    d.layer("0", 7); d.layer("PARTS", 2)
    # leaf block: a bolt drawn ByBlock so it inherits the INSERT colour
    d.block("BOLT", (0, 0), [
        circle(0, 0, 5, layer="0", color=0),
        lwpoly([(-4, -4), (4, -4), (4, 4), (-4, 4)], closed=True, layer="0", color=0),
        line(-6, 0, 6, 0, layer="0", color=0),
    ])
    # nested block containing 4 bolts
    d.block("PLATE", (0, 0), [
        lwpoly([(-20, -20), (20, -20), (20, 20), (-20, 20)], closed=True, layer="PARTS"),
        insert("BOLT", -12, -12), insert("BOLT", 12, -12),
        insert("BOLT", -12, 12), insert("BOLT", 12, 12),
    ])
    d.add(insert("PLATE", 0, 0))
    d.add(insert("PLATE", 80, 0, sx=1.5, sy=1.5, color=1))
    d.add(insert("PLATE", 180, 0, rot=30.0, color=3))
    d.add(insert("PLATE", 0, 90, sx=-1.0, sy=1.0, color=5))          # mirrored
    d.add(insert("PLATE", 90, 90, sx=2.0, sy=0.5, rot=15.0, color=6))  # non-uniform + rot
    d.add(insert("BOLT", 0, 200, cols=8, rows=3, cspace=20, rspace=20, color=4))  # array
    d.add(insert("MISSING_BLOCK", 300, 300))                          # dangling ref
    return d


def f_text():
    d = Dxf()
    d.layer("0", 7); d.layer("NOTES", 2)
    for i, (ha, va, lbl) in enumerate([
        (0, 0, "left/base"), (1, 0, "center/base"), (2, 0, "right/base"),
        (0, 1, "left/bottom"), (1, 2, "center/middle"), (2, 3, "right/top"),
    ]):
        y = 100 - i * 20
        d.add(line(0, y, 0, y, color=1))
        d.add(point(0, y, color=1))
        d.add(text(0, y, 5, lbl, halign=ha, valign=va,
                   x2=(0 if ha or va else None), y2=(y if ha or va else None), layer="NOTES"))
    d.add(text(0, 130, 8, "Rotated 30deg", rot=30.0, color=3))
    d.add(text(0, 160, 8, "UPPER & lower 0123", color=5))
    d.add(mtext(120, 100, 6, "MText line one\\Pline two\\Pline three", width=80, attach=1))
    d.add(mtext(120, 40, 6, "{\\C1;colored} and \\L underlined\\l tokens", width=80, attach=5, color=4))
    d.add(text(120, 0, 6, "", color=2))          # empty string
    return d


def f_ocs():
    d = Dxf()
    d.layer("0", 7)
    # WCS reference cross
    d.add(line(-60, 0, 60, 0)); d.add(line(0, -60, 0, 60))
    # circle in a plane whose normal is -Z: mirrors X
    d.add(circle(30, 0, 20, color=1, normal=(0, 0, -1)))
    # arbitrary axis algorithm: normal close to +Z but not exactly (below 1/64)
    d.add(arc(0, 30, 20, 0, 180, color=3, normal=(0.001, 0.001, 1.0)))
    # A mirrored arc whose start angle is NOT zero. Under -Z extrusion the OCS x axis is negated,
    # so the image of OCS angle t is at (pi - t), not (pi + t). An arc starting at 0 cannot tell
    # the two apart, which is why this one starts at 45.
    d.add(arc(0, -40, 20, 45, 135, color=2, normal=(0, 0, -1)))
    # Just above the 1/64 threshold, so the algorithm takes the world-Z branch instead.
    d.add(arc(60, -40, 15, 0, 90, color=4, normal=(0.02, 0.0, 1.0)))
    # normal in the XY plane -> the "Wy cross" branch of the AAA
    d.add(circle(0, 0, 25, color=5, normal=(1.0, 0.0, 0.0)))
    d.add(lwpoly([(0, 0), (40, 0), (40, 40)], color=6, normal=(0, 0, -1)))
    d.add(lwpoly([(0, 0), (30, 0), (30, 30)], color=2, elev=10.0, normal=(0, 0, 1)))
    # 3D geometry
    d.add(line(0, 0, 50, 50, z1=0.0, z2=40.0))
    d.add(face3d([(0, 0, 0), (40, 0, 0), (40, 40, 20), (0, 40, 20)], color=4))
    d.add(solid([(60, 0), (90, 0), (60, 30), (90, 30)], color=1))   # note DXF's 3/4 swap
    return d


def f_empty():
    d = Dxf()
    d.layer("0", 7)
    return d


def f_large(n_cells=140):
    """~120k entities: a big hatched grid + arcs, for perf work."""
    d = Dxf()
    d.layer("0", 7); d.layer("GRID", 8); d.layer("DETAIL", 4)
    step = 10.0
    for i in range(n_cells + 1):
        d.add(line(0, i * step, n_cells * step, i * step, layer="GRID"))
        d.add(line(i * step, 0, i * step, n_cells * step, layer="GRID"))
    for r in range(n_cells):
        for c in range(n_cells):
            x, y = c * step + step / 2, r * step + step / 2
            k = (r * n_cells + c) % 6
            if k == 0:
                d.add(circle(x, y, step * 0.35, layer="DETAIL", color=1 + (c % 7)))
            elif k == 1:
                d.add(arc(x, y, step * 0.35, 0, 270, layer="DETAIL", color=1 + (r % 7)))
            elif k == 2:
                d.add(lwpoly([(x - 3, y - 3, 0.4), (x + 3, y - 3, 0), (x + 3, y + 3, 0.4),
                              (x - 3, y + 3, 0)], closed=True, layer="DETAIL"))
            elif k == 3:
                d.add(line(x - 4, y - 4, x + 4, y + 4, layer="DETAIL", color=2))
                d.add(line(x - 4, y + 4, x + 4, y - 4, layer="DETAIL", color=2))
            elif k == 4:
                d.add(ellipse(x, y, 4, 0, 0.5, layer="DETAIL", color=6))
            else:
                d.add(point(x, y, layer="DETAIL", color=3))
    return d


def f_annotation():
    """Attributes on an INSERT, control codes, Middle justification, and construction lines."""
    d = Dxf()
    d.layer("0", 7); d.layer("TAGS", 2); d.layer("CONSTRUCTION", 8, "DASHED")
    d.block("PART", (0, 0), [
        lwpoly([(-10, -6), (10, -6), (10, 6), (-10, 6)], closed=True, layer="0", color=0),
        chunk((0, "ATTDEF"), (8, "0"), (10, -8.0), (20, -2.0), (30, 0.0), (40, 3.0),
              (1, ""), (3, "Part tag"), (2, "TAG"), (70, 0)),
    ])
    d.add(insert("PART", 0, 0, layer="TAGS", color=2,
                 attribs=[("TAG", "PUMP-101", -8.0, -2.0, 3.0)]))
    d.add(insert("PART", 40, 0, layer="TAGS", color=3,
                 attribs=[("TAG", "%%c50 BORE", 32.0, -2.0, 3.0)]))
    d.add(insert("PART", 80, 0, layer="TAGS"))              # no attributes at all
    # Control codes in plain TEXT, and the justification that centres in both directions.
    d.add(text(0, 20, 4, "%%c44 %%p0.1 45%%d", layer="0", color=1))
    d.add(text(0, 30, 4, "middle", halign=4, valign=0, x2=0.0, y2=30.0, layer="0", color=5))
    # Construction lines: infinite, and must not drag the drawing extent out with them.
    d.add(ray(0, 0, 1.0, 1.0, layer="CONSTRUCTION"))
    d.add(xline(0, 10, 1.0, 0.0, layer="CONSTRUCTION"))
    return d


def f_latin1():
    """Pre-R2007, so the text is Windows-1252 rather than UTF-8."""
    d = Dxf(version="AC1015", codepage="ANSI_1252")
    d.layer("0", 7)
    d.add(text(0, 0, 5, "Bohrung \u00d820 f\u00fcr M6"))
    d.add(text(0, 20, 5, "Ma\u00dfstab 1:2"))
    return d


def f_malformed():
    """Structurally odd but recoverable content."""
    d = Dxf()
    d.layer("0", 7)
    d.add(circle(0, 0, 0.0))                     # zero radius
    d.add(circle(20, 0, -5.0))                   # negative radius
    d.add(arc(40, 0, 10, 45, 45))                # zero sweep
    d.add(line(60, 0, 60, 0))                    # zero length
    d.add(lwpoly([], closed=True))               # no vertices
    d.add(line(1e9, 1e9, -1e9, -1e9))            # huge coords
    d.add(circle(0, 0, 1e-9, color=2))           # sub-epsilon
    d.add(text(0, 20, 0.0, "zero height"))       # zero text height
    # A zero-scale INSERT of a block that DOES exist: naming a missing one would exit on the
    # missing-block branch and never reach the scale check.
    d.block("REAL", (0, 0), [circle(0, 0, 5, layer="0")])
    d.add(insert("REAL", 0, 40, sx=0.0, sy=0.0))
    d.add(insert("REAL", 40, 40, sx=1.0, sy=1.0))   # the same block at a usable scale, as a control
    d.add(spline([(0, 60), (10, 70)], [0, 0, 1, 1], 3))  # degree > n_ctrl-1
    return d



def f_showcase():
    """A plausible mechanical drawing: a bearing bracket with a title block.

    Exists mainly so the README screenshot shows something a CAD user recognises, but it also
    exercises blocks, bulged polylines, text justification and layer colours together.
    """
    import math as m
    d = Dxf()
    d.layer("0", 7)
    OUT = d.layer("OUTLINE", 7)
    HID = d.layer("HIDDEN", 8, "DASHED")
    CEN = d.layer("CENTRE", 4, "CENTER")
    DIM = d.layer("DIMENSION", 3)
    HAT = d.layer("HATCH", 8)
    TXT = d.layer("TEXT", 2)
    BRD = d.layer("BORDER", 7)

    d.block("BOLT", (0, 0), [
        circle(0, 0, 4.0, layer="0", color=0),
        circle(0, 0, 2.6, layer="0", color=0),
        line(-5.2, 0, 5.2, 0, layer="0", color=0),
        line(0, -5.2, 0, 5.2, layer="0", color=0),
    ])

    # ---- front view: a bracket body with a bored boss and rounded corners
    b = m.tan(m.radians(90) / 4)
    d.add(lwpoly([(20, 40, 0), (20, 130, b), (28, 138, 0), (112, 138, b),
                  (120, 130, 0), (120, 40, b), (112, 32, 0), (28, 32, b)],
                 closed=True, layer="OUTLINE"))
    d.add(circle(70, 96, 30, layer="OUTLINE"))
    d.add(circle(70, 96, 22, layer="OUTLINE"))
    d.add(circle(70, 96, 34, layer="HIDDEN"))
    # bolt circle
    for i in range(6):
        a = m.radians(90 + i * 60)
        d.add(insert("BOLT", 70 + 44 * m.cos(a), 96 + 44 * m.sin(a), layer="OUTLINE"))
    d.add(circle(70, 96, 44, layer="CENTRE"))
    # centre marks
    d.add(line(28, 96, 112, 96, layer="CENTRE"))
    d.add(line(70, 24, 70, 146, layer="CENTRE"))
    # mounting slots, as bulged polylines
    for x in (36, 104):
        d.add(lwpoly([(x - 5, 44, 1.0), (x + 5, 44, 0), (x + 5, 60, 1.0), (x - 5, 60, 0)],
                     closed=True, layer="OUTLINE"))

    # ---- section view on the right, with hatching
    sx = 150
    d.add(lwpoly([(sx, 40), (sx + 46, 40), (sx + 46, 138), (sx, 138)], closed=True, layer="OUTLINE"))
    d.add(lwpoly([(sx + 14, 66), (sx + 32, 66), (sx + 32, 112), (sx + 14, 112)],
                 closed=True, layer="OUTLINE"))
    # 45-degree section hatch, clipped to the part's cross-section (outer box minus the bore).
    def hatch_span(x0, y0, x1, y1, step, box, hole=None):
        import math as mm
        lo = box[0] + box[1]                      # x + y at the near corner
        hi = box[2] + box[3]
        c = lo
        while c <= hi:
            # The line x + y = c, clipped to the box, then split around the hole.
            pts = []
            for (px, py) in ((box[0], c - box[0]), (box[2], c - box[2]),
                             (c - box[1], box[1]), (c - box[3], box[3])):
                if box[0] - 1e-9 <= px <= box[2] + 1e-9 and box[1] - 1e-9 <= py <= box[3] + 1e-9:
                    pts.append((px, py))
            if len(pts) >= 2:
                pts.sort()
                a, b2 = pts[0], pts[-1]
                if mm.dist(a, b2) > 1e-6:
                    segs = [(a, b2)]
                    if hole:
                        segs = clip_out(a, b2, hole)
                    for (s0, s1) in segs:
                        d.add(line(s0[0], s0[1], s1[0], s1[1], layer="HATCH"))
            c += step
        _ = (x0, y0, x1, y1)

    def clip_out(a, b2, hole):
        """Split segment a-b around an axis-aligned hole, returning the parts outside it."""
        hx0, hy0, hx1, hy1 = hole
        dx, dy = b2[0] - a[0], b2[1] - a[1]
        t0, t1 = 0.0, 1.0
        for (p, q) in ((-dx, a[0] - hx0), (dx, hx1 - a[0]), (-dy, a[1] - hy0), (dy, hy1 - a[1])):
            if p == 0:
                if q < 0:
                    return [(a, b2)]              # parallel and outside: nothing to remove
            else:
                r = q / p
                if p < 0:
                    if r > t1: return [(a, b2)]
                    t0 = max(t0, r)
                else:
                    if r < t0: return [(a, b2)]
                    t1 = min(t1, r)
        if t0 >= t1:
            return [(a, b2)]
        at = lambda t: (a[0] + dx * t, a[1] + dy * t)
        out = []
        if t0 > 1e-9: out.append((a, at(t0)))
        if t1 < 1 - 1e-9: out.append((at(t1), b2))
        return out

    hatch_span(0, 0, 0, 0, 5.0,
               (sx, 40, sx + 46, 138),
               (sx + 14, 66, sx + 32, 112))
    d.add(line(sx - 6, 96, sx + 52, 96, layer="CENTRE"))

    # ---- dimension-ish annotation (extension + arrow lines)
    def dim_h(x0, x1, y, label):
        d.add(line(x0, y - 6, x0, y + 6, layer="DIMENSION"))
        d.add(line(x1, y - 6, x1, y + 6, layer="DIMENSION"))
        d.add(line(x0, y, x1, y, layer="DIMENSION"))
        d.add(lwpoly([(x0, y), (x0 + 4, y + 1.4), (x0 + 4, y - 1.4)], closed=True, layer="DIMENSION"))
        d.add(lwpoly([(x1, y), (x1 - 4, y + 1.4), (x1 - 4, y - 1.4)], closed=True, layer="DIMENSION"))
        d.add(text((x0 + x1) / 2, y + 3, 5, label, halign=1, x2=(x0 + x1) / 2, y2=y + 3, layer="TEXT"))

    dim_h(20, 120, 20, "100")
    dim_h(sx, sx + 46, 172, "46")
    d.add(text(70, 152, 6, "BEARING BRACKET", halign=1, x2=70, y2=152, layer="TEXT"))
    d.add(text(sx + 23, 152, 6, "SECTION A-A", halign=1, x2=sx + 23, y2=152, layer="TEXT"))
    d.add(text(70, 88, 5, "\u00d844 H7", halign=1, x2=70, y2=88, layer="TEXT"))
    d.add(text(70, 12, 4, "TOLERANCE \u00b10.1 UNLESS STATED", halign=1, x2=70, y2=12, layer="TEXT"))

    # ---- sheet border and title block
    d.add(lwpoly([(0, 0), (250, 0), (250, 180), (0, 180)], closed=True, layer="BORDER"))
    d.add(lwpoly([(4, 4), (246, 4), (246, 176), (4, 176)], closed=True, layer="BORDER"))
    d.add(lwpoly([(156, 4), (246, 4), (246, 34), (156, 34)], closed=True, layer="BORDER"))
    d.add(line(156, 24, 246, 24, layer="BORDER"))
    d.add(line(156, 14, 246, 14, layer="BORDER"))
    d.add(line(201, 14, 201, 24, layer="BORDER"))
    d.add(text(159, 27, 4.0, "BEARING BRACKET", layer="TEXT"))
    d.add(text(159, 17, 3.2, "SCALE 1:1", layer="TEXT"))
    d.add(text(204, 17, 3.2, "SHEET 1/1", layer="TEXT"))
    d.add(text(159, 7.5, 3.0, "opendxfviewer sample", layer="TEXT"))
    return d

FIXTURES = {
    "basic.dxf": f_basic,
    "polylines.dxf": f_polylines,
    "curves.dxf": f_curves,
    "blocks.dxf": f_blocks,
    "text.dxf": f_text,
    "ocs_3d.dxf": f_ocs,
    "empty.dxf": f_empty,
    "malformed.dxf": f_malformed,
    "showcase.dxf": f_showcase,
    "annotation.dxf": f_annotation,
    "latin1.dxf": f_latin1,
}

if __name__ == "__main__":
    OUT.mkdir(parents=True, exist_ok=True)
    for name, fn in FIXTURES.items():
        dxf = fn()
        txt = dxf.render()
        if dxf.codepage == "ANSI_1252" and dxf.version < "AC1021":
            (OUT / name).write_bytes(txt.encode("cp1252", errors="replace"))
        else:
            (OUT / name).write_text(txt, encoding="utf-8")
        print(f"{name:16} {len(txt):>10,} bytes")
    import sys
    # Perf fixtures are git-ignored: generate them on demand, they are large.
    if "--large" in sys.argv or "--all" in sys.argv:
        txt = f_large(140).render()          # ~23k entities, 1.8 MB
        (OUT / "large.dxf").write_text(txt)
        print(f"{'large.dxf':16} {len(txt):>10,} bytes")
    if "--huge" in sys.argv or "--all" in sys.argv:
        txt = f_large(380).render()          # ~169k entities, 14 MB
        (OUT / "huge.dxf").write_text(txt)
        print(f"{'huge.dxf':16} {len(txt):>10,} bytes")
