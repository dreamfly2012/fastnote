"""按窗口标题找 fastnote 窗口并截图（比按 pid 稳）。

用法: python tools/shot_title.py [输出路径]
"""
import ctypes
import ctypes.wintypes as wt
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import shot

u = ctypes.windll.user32

found = []
WNDENUMPROC = ctypes.WINFUNCTYPE(ctypes.c_bool, wt.HWND, wt.LPARAM)


def cb(h, l):
    if u.IsWindowVisible(h):
        n = u.GetWindowTextLengthW(h)
        b = ctypes.create_unicode_buffer(n + 1)
        u.GetWindowTextW(h, b, n + 1)
        if "fastnote" in b.value.lower():
            found.append((int(h), b.value))
    return True


u.EnumWindows(WNDENUMPROC(cb), 0)
print("windows:", found)
if not found:
    raise SystemExit("no fastnote window found")

out = sys.argv[1] if len(sys.argv) > 1 else "shot.png"
hwnd, title = found[0]
w, h = shot.capture(hwnd, out)
print("captured %dx%d -> %s (title=%r)" % (w, h, out, title))
