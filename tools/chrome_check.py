"""界面外框验证：默认无边框界面 -> 鼠标移到窗口顶端唤出工具条 -> 移开自动收起。

用法（必须与启动应用在同一条命令里，进程会随 shell 被回收）：
    taskkill /F /IM fastnote.exe; target-app/debug/fastnote.exe <file> & sleep 8; python tools/chrome_check.py
"""
import ctypes, ctypes.wintypes as wt, sys, time

sys.path.insert(0, 'tools')
import shot

u = ctypes.windll.user32


def find_window():
    Proc = ctypes.WINFUNCTYPE(ctypes.c_bool, wt.HWND, wt.LPARAM)
    found = []

    def cb(hwnd, lparam):
        if u.IsWindowVisible(hwnd):
            n = u.GetWindowTextLengthW(hwnd)
            buf = ctypes.create_unicode_buffer(n + 1)
            u.GetWindowTextW(hwnd, buf, n + 1)
            if 'fastnote' in buf.value.lower():
                found.append(int(hwnd))
        return True

    u.EnumWindows(Proc(cb), 0)
    return found[0] if found else None


def ensure_front(hwnd, r):
    """把 fastnote 拉到前台并校验：桌面上常有别的窗口压着，
    SetForegroundWindow 单独用不可靠（非前台进程调用会被系统忽略），
    先轻按一下 ALT 解开前台锁再切，最后用截图亮度确认前台确实是 fastnote。"""
    from PIL import Image

    for attempt in (1, 2, 3):
        u.ShowWindow(hwnd, 9)  # SW_RESTORE
        u.keybd_event(0x12, 0, 0, 0)
        u.keybd_event(0x12, 0, 2, 0)
        u.BringWindowToTop(hwnd)
        u.SetForegroundWindow(hwnd)
        time.sleep(0.8)
        fg = u.GetForegroundWindow()
        shot.capture(hwnd, 'shot_ui_frontcheck.png')
        img = Image.open('shot_ui_frontcheck.png').convert('RGB')
        px_img = img.load()
        w, h = img.size
        # 编辑区（正文背景）应当是接近白的浅色；深色说明前台是别的窗口
        samples = [px_img[int(w * 0.6), int(h * 0.5)], px_img[int(w * 0.5), int(h * 0.75)]]
        bright = all(sum(c) > 3 * 200 for c in samples)
        print(f"  attempt {attempt}: fg={fg == hwnd} samples={samples} bright={bright}")
        if fg == hwnd and bright:
            return True
    return False


def main():
    hwnd = find_window()
    if not hwnd:
        print("no window")
        return 1
    r = wt.RECT()
    u.GetWindowRect(hwnd, ctypes.byref(r))
    print("window", r.left, r.top, r.right - r.left, r.bottom - r.top)
    if not ensure_front(hwnd, r):
        print("fastnote 不在前台（可能有别的窗口压着），放弃以免误操作")
        return 1

    def move(x, y, wait=1.2):
        u.SetCursorPos(r.left + int(x), r.top + int(y))
        time.sleep(wait)

    # 1) 默认：鼠标停在正文中间 -> 顶栏与两侧都应不可见
    move(700, 500)
    print("default:", shot.capture(hwnd, 'shot_ui_default.png'))

    # 2) 鼠标移到客户端最上沿（窗口坐标里前面还有 ~32px 标题栏）-> 工具条浮现
    move(600, 40)
    print("chrome:", shot.capture(hwnd, 'shot_ui_chrome.png'))

    # 3) 移开 -> 自动收起
    move(700, 500)
    print("hidden:", shot.capture(hwnd, 'shot_ui_hidden.png'))

    # 4) 键盘切左右侧栏（先点一下正文拿焦点，否则按键发到别的窗口）
    def click(x, y, wait=1.0):
        u.SetCursorPos(r.left + int(x), r.top + int(y))
        time.sleep(0.2)
        u.mouse_event(0x0002, 0, 0, 0, 0)
        u.mouse_event(0x0004, 0, 0, 0, 0)
        time.sleep(wait)

    def key(*vk):
        for k in vk:
            u.keybd_event(k, 0, 0, 0)
        for k in reversed(vk):
            u.keybd_event(k, 0, 2, 0)
        time.sleep(1.2)

    VK_CTRL, VK_SHIFT = 0x11, 0x10
    click(700, 500)
    key(VK_CTRL, 0xDC)          # Ctrl+\ 左侧栏
    print("sidebar:", shot.capture(hwnd, 'shot_ui_sidebar.png'))
    key(VK_CTRL, VK_SHIFT, 0x52)  # Ctrl+Shift+R 右侧知识面板
    print("panels:", shot.capture(hwnd, 'shot_ui_panels.png'))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
