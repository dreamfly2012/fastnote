"""浮层回归验证：每个浮层「打开 -> Esc 关闭」「打开 -> 点空白处关闭」。

判定为主、判据要抗干扰。踩过的坑决定了现在的做法：

1. **用「遮罩区域亮度」当主判据**，而不是纯像素/分块差异。
   浮层打开的标志是那张铺满窗口的压暗遮罩，它会让面板区域的灰度均值明显下降
   （实测 -9 ~ -61）；关掉后回落到与基准完全一致（实测 0.0）。
   这个判据对「正文重排」「侧栏开关」这类与浮层无关的变化不敏感。
   分块差异只作为参考列打印出来。

2. **空白探测点只能用左上角 (6,6)**：
   * 底部不能用 —— 本窗口比屏幕还高（客户区 y=332 + 高 800 > 1080），
     靠底部的点会点到任务栏，浮层当然关不掉；
   * 靠顶部会触发顶端悬浮工具条的热区；
   * 左上角会顺带碰到左侧栏把手，所以判据必须对侧栏不敏感（见第 1 条）。

3. **基准图每个浮层单独拍**，不能用一张全局基准：点空白会留下光标/hover 残影。

4. 组合键偶发被窗口激活吃掉，所以打开带重试；重试仍失败就如实报「打开失败」。

用法（必须与启动应用写在同一条命令里，进程随 shell 回收）：
    taskkill /F /IM fastnote.exe; target-app/debug/fastnote.exe sample-vault/项目计划.md & \
      sleep 9; python tools/overlay_check.py --kill

只对已有截图复算（不驱动 GUI，秒级）：
    python tools/overlay_check.py --report
"""
import ctypes, ctypes.wintypes as wt, sys, time

sys.path.insert(0, 'tools')
import shot
from PIL import Image, ImageChops, ImageStat

u = ctypes.windll.user32

VK_CTRL, VK_SHIFT, VK_ESC, VK_ALT = 0x11, 0x10, 0x1B, 0x12

# (名字, 打开用的按键序列)
# 注意 settings：本机 `Ctrl+,` 的按键在平台层就被吞掉（见 main.rs 里 bindings 的注释），
# 真正能送达的是 `Ctrl+Shift+,`，所以这里必须用它。
OVERLAYS = [
    ('palette', (VK_CTRL, 0x4B)),                     # Ctrl+K 命令面板
    ('settings', (VK_CTRL, VK_SHIFT, 0xBC)),          # Ctrl+Shift+,  AI 设置
    ('help', (VK_CTRL, VK_SHIFT, 0xBF)),              # Ctrl+Shift+?  快捷键
    ('search', (VK_CTRL, VK_SHIFT, 0x46)),            # Ctrl+Shift+F 全库搜索
    ('graph', (VK_CTRL, VK_SHIFT, 0x47)),             # Ctrl+Shift+G 关系图谱
    ('chat', (VK_CTRL, VK_SHIFT, 0x51)),              # Ctrl+Shift+Q 库问答
    ('board', (VK_CTRL, VK_SHIFT, 0x42)),             # Ctrl+Shift+B 白板
    ('history', (VK_CTRL, VK_SHIFT, 0x48)),           # Ctrl+Shift+H 历史版本
]

BLOCK = 8
BLOCK_DIFF = 12

# 判据阈值
OPEN_DIM = 5.0    # 打开时区域均值至少要比基准暗这么多（遮罩生效）
CLOSED_TOL = 5.0  # 关闭后区域均值与基准的差允许这么大

# 采样区域（图像坐标系）：面板所在的中部，避开左侧栏条带与顶端热区
BOX_X0, BOX_Y0, BOX_Y1 = 300, 120, 700


def find_window():
    Proc = ctypes.WINFUNCTYPE(ctypes.c_bool, wt.HWND, wt.LPARAM)
    found = []

    def cb(hwnd, lparam):
        if u.IsWindowVisible(hwnd):
            n = u.GetWindowTextLengthW(hwnd)
            b = ctypes.create_unicode_buffer(n + 1)
            u.GetWindowTextW(hwnd, b, n + 1)
            if 'fastnote' in b.value.lower():
                found.append(int(hwnd))
        return True

    u.EnumWindows(Proc(cb), 0)
    return found


def box(size):
    """按图片宽度算采样框，避免写死尺寸。"""
    w, _ = size
    return (BOX_X0, BOX_Y0, min(w - 40, 1400), BOX_Y1)


def region_mean(path, bx):
    im = Image.open(path).convert('L')
    return ImageStat.Stat(im.crop(bx)).mean[0]


def block_diff(a, b, bx):
    """采样区内差异的分块计数，仅作参考（对重排敏感）。"""
    ia = Image.open(a).convert('L').crop(bx)
    ib = Image.open(b).convert('L').crop(bx)
    d = ImageChops.difference(ia, ib)
    w, h = d.size
    px = d.load()
    n = 0
    for by in range(0, h - BLOCK + 1, BLOCK):
        for pxx in range(0, w - BLOCK + 1, BLOCK):
            s = 0
            for y in range(by, by + BLOCK):
                for x in range(pxx, pxx + BLOCK):
                    s += px[x, y]
            if s / (BLOCK * BLOCK) > BLOCK_DIFF:
                n += 1
    return n


def score_one(name, bx):
    """返回 (基准均值, 打开, Esc后, 点空白后) 四张图的区域均值。"""
    p = lambda suf: f'shot_oc_{name}_{suf}.png'
    return tuple(region_mean(p(s), bx) for s in ('base', 'open', 'esc', 'click'))


def report():
    """离线复算：只读已有的 shot_oc_*.png，不驱动 GUI。"""
    try:
        size = Image.open('shot_oc_palette_base.png').size
    except FileNotFoundError:
        print('没有找到 shot_oc_*.png，请先跑一次完整验证')
        return 1
    bx = box(size)

    print(f'{"浮层":<10}{"开-基":>8}{"关-基":>8}{"点-基":>8}{"打开块":>8}   判定')
    print('-' * 62)
    bad = []
    for name, _ in OVERLAYS:
        try:
            b, o, e, c = score_one(name, bx)
        except FileNotFoundError as ex:
            print(f'{name:<10}  缺文件：{ex.filename}')
            bad.append(name)
            continue
        d_open, d_esc, d_click = o - b, e - b, c - b
        blk = block_diff(f'shot_oc_{name}_base.png', f'shot_oc_{name}_open.png', bx)
        if d_open > -OPEN_DIM:
            verdict = '打开失败（遮罩没出现）'
            bad.append(name)
        else:
            ok_esc = abs(d_esc) <= CLOSED_TOL
            ok_click = abs(d_click) <= CLOSED_TOL
            verdict = f'Esc {"OK" if ok_esc else "NG"}   点击 {"OK" if ok_click else "NG"}'
            if not (ok_esc and ok_click):
                bad.append(name)
        print(f'{name:<10}{d_open:>8.1f}{d_esc:>8.1f}{d_click:>8.1f}{blk:>8}   {verdict}')

    print('\n不通过的浮层：', bad if bad else '无')
    print('（开-基/关-基/点-基 是采样区灰度均值相对基准的差；打开应明显为负（遮罩压暗），'
          '关闭应回到 0 附近）')
    return 0 if not bad else 1


def main():
    hwnds = find_window()
    if not hwnds:
        print('no window')
        return 1
    hwnd = hwnds[0]

    cr = wt.RECT()
    u.GetClientRect(hwnd, ctypes.byref(cr))
    org = wt.POINT(0, 0)
    u.ClientToScreen(hwnd, ctypes.byref(org))
    cx0, cy0 = org.x, org.y
    cw, ch = cr.right - cr.left, cr.bottom - cr.top
    print(f'客户区 origin=({cx0},{cy0}) size={cw}x{ch}')

    mid = (cx0 + cw // 2, cy0 + ch // 2)
    blank = (cx0 + 6, cy0 + 6)   # 见文件头说明：只有这个点可靠

    u.ShowWindow(hwnd, 9)
    u.keybd_event(VK_ALT, 0, 0, 0)
    u.keybd_event(VK_ALT, 0, 2, 0)
    u.BringWindowToTop(hwnd)
    u.SetForegroundWindow(hwnd)
    time.sleep(1.2)

    def park(wait=0.9):
        u.SetCursorPos(*mid)
        time.sleep(wait)

    def combo(*vk, wait=1.5):
        for k in vk:
            u.keybd_event(k, 0, 0, 0)
        time.sleep(0.15)
        for k in reversed(vk):
            u.keybd_event(k, 0, 2, 0)
        time.sleep(wait)

    def esc(wait=1.5):
        u.keybd_event(VK_ESC, 0, 0, 0)
        time.sleep(0.1)
        u.keybd_event(VK_ESC, 0, 2, 0)
        time.sleep(wait)

    def click_at(pos, wait=1.5):
        u.SetCursorPos(*pos)
        time.sleep(0.35)
        u.mouse_event(0x0002, 0, 0, 0, 0)
        u.mouse_event(0x0004, 0, 0, 0, 0)
        time.sleep(wait)

    def capture(name):
        park()
        shot.capture(hwnd, name)

    click_at(mid, wait=1.0)

    def open_overlay(prefix, keys, base):
        for attempt in range(3):
            combo(*keys)
            capture(f'shot_oc_{prefix}.png')
            if region_mean(f'shot_oc_{prefix}.png', bx) - region_mean(base, bx) <= -OPEN_DIM:
                return True
            if attempt < 2:
                print(f'    (重试打开 {attempt + 2}/3 …)')
        return False

    bx = box(shot.capture(hwnd, 'shot_oc_probe_size.png'))

    for name, keys in OVERLAYS:
        base = f'shot_oc_{name}_base.png'
        capture(base)
        open_overlay(f'{name}_open', keys, base)
        esc()
        capture(f'shot_oc_{name}_esc.png')
        if open_overlay(f'{name}_open2', keys, base):
            click_at(blank)
            capture(f'shot_oc_{name}_click.png')
        print(f'  {name} 采集完成')

    return report()


if __name__ == '__main__':
    if '--report' in sys.argv:
        raise SystemExit(report())
    code = main()
    if '--kill' in sys.argv:
        import os
        os.system('taskkill /F /IM fastnote.exe >/dev/null 2>&1')
    raise SystemExit(code)
