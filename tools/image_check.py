"""图片渲染实跑验证：
step0 基准（demo.png，绝对文件名）
step1 下滚看第二张图（相对路径 ../docs/ui/...）
step2 点第一张图 → 光标进块 → 回退源码显示
"""
import ctypes
import ctypes.wintypes as wt
import os
import sys
import time

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
            found.append(int(h))
    return True


u.EnumWindows(WNDENUMPROC(cb), 0)
if not found:
    raise SystemExit("no fastnote window")
hwnd = found[0]

r = wt.RECT()
u.GetWindowRect(hwnd, ctypes.byref(r))


def click(x, y, wait=1.2):
    u.SetCursorPos(r.left + x, r.top + y)
    time.sleep(0.25)
    u.mouse_event(0x0002, 0, 0, 0, 0)
    u.mouse_event(0x0004, 0, 0, 0, 0)
    time.sleep(wait)


def wheel(delta, wait=1.2):
    u.SetCursorPos(r.left + 600, r.top + 400)
    time.sleep(0.2)
    u.mouse_event(0x0800, 0, 0, delta, 0)
    time.sleep(wait)


# step0: 基准，第一张图可见
shot.capture(hwnd, "img_step0.png")
# step1: 滚到底，第二张图（相对路径）应出现
wheel(-4400)
# 滚过头了会停在文档底部，回滚一点把第二张图带进视口
wheel(720)
shot.capture(hwnd, "img_step1.png")
# step2: 回到顶部，点第一张图中心 → 块进入活动态 → 源码
wheel(8800)
click(540, 420)
shot.capture(hwnd, "img_step2.png")
print("done")
