import math, sys
out = sys.argv[1]

def trail(cx, cy, r, a0, a1, w0, w1, n=120):
    """A C-shaped baton trail: an arc from angle a0 (the tip) round to a1, width w0 at the tip easing to w1."""
    outer, inner = [], []
    for i in range(n + 1):
        t = i / n
        a = math.radians(a0 + (a1 - a0) * t)
        w = w0 + (w1 - w0) * (t ** 0.8)
        for lst, rr in ((outer, r + w / 2), (inner, r - w / 2)):
            lst.append((cx + rr * math.cos(a), cy - rr * math.sin(a)))
    pts = outer + inner[::-1]
    d = "M" + " L".join(f"{x:.1f},{y:.1f}" for x, y in pts) + "Z"
    tip = (cx + r * math.cos(math.radians(a0)), cy - r * math.sin(math.radians(a0)))
    tail = (cx + r * math.cos(math.radians(a1)), cy - r * math.sin(math.radians(a1)))
    return d, tip, tail

def mark(size=512, cx=262, cy=256, r=148):
    # tip at upper right (42°), sweeping counter-clockwise over the top, round the left, to lower right (318°)
    d, tip, tail = trail(cx, cy, r, 42, 318, 46, 11)
    return d, tip, tail

d, tip, tail = mark()
defs = f'''<defs>
    <linearGradient id="bg" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="#211A45"/><stop offset="1" stop-color="#0F0D1F"/>
    </linearGradient>
    <linearGradient id="gold" gradientUnits="userSpaceOnUse" x1="{tip[0]:.0f}" y1="{tip[1]:.0f}" x2="{tail[0]:.0f}" y2="{tail[1]:.0f}">
      <stop offset="0" stop-color="#FFD978"/><stop offset="0.55" stop-color="#E9A93B"/><stop offset="1" stop-color="#C9822A" stop-opacity="0.5"/>
    </linearGradient>
    <radialGradient id="glow"><stop offset="0" stop-color="#FFF6DC" stop-opacity="0.9"/><stop offset="1" stop-color="#FFD978" stop-opacity="0"/></radialGradient>
  </defs>'''
tipdots = f'''<circle cx="{tip[0]:.1f}" cy="{tip[1]:.1f}" r="58" fill="url(#glow)"/>
  <circle cx="{tip[0]:.1f}" cy="{tip[1]:.1f}" r="20" fill="#FFFFFF"/>'''

icon = f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512" width="512" height="512">
  <title>Conductor</title>
  {defs}
  <rect width="512" height="512" rx="112" fill="url(#bg)"/>
  <path d="{d}" fill="url(#gold)"/>
  {tipdots}
</svg>
'''
# the mark alone, no tile (for light or dark backgrounds)
mark_only = f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="80 70 380 380" width="380" height="380">
  <title>Conductor</title>
  {defs}
  <path d="{d}" fill="url(#gold)"/>
  {tipdots.replace('#FFFFFF', '#FFF3D0')}
</svg>
'''
# one-colour version (favicons, print, monochrome)
mono = f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="80 70 380 380" width="380" height="380">
  <title>Conductor</title>
  <path d="{d}" fill="currentColor"/>
  <circle cx="{tip[0]:.1f}" cy="{tip[1]:.1f}" r="22" fill="currentColor"/>
</svg>
'''
def lockup(text_color, bg):
    return f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1240 400" width="1240" height="400">
  <title>Conductor</title>
  {defs}
  {'' if bg is None else f'<rect width="1240" height="400" fill="{bg}"/>'}
  <g transform="translate(40,44) scale(0.61)">
    <rect width="512" height="512" rx="112" fill="url(#bg)"/>
    <path d="{d}" fill="url(#gold)"/>
    {tipdots}
  </g>
  <text x="400" y="246" font-family="Inter" font-weight="700" font-size="150" letter-spacing="-4" fill="{text_color}">Conductor</text>
</svg>
'''
open(f"{out}/conductor-icon.svg", "w").write(icon)
open(f"{out}/conductor-mark.svg", "w").write(mark_only)
open(f"{out}/conductor-mono.svg", "w").write(mono)
open(f"{out}/conductor-lockup-dark.svg", "w").write(lockup("#F4F1FF", "#0E0F13"))
open(f"{out}/conductor-lockup-light.svg", "w").write(lockup("#16132B", "#F7F7FA"))
