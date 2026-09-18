"""界面美化验证：默认净界面 -> 顶端浮现工具条 -> 逐个浮层截图。

用法（启动应用与脚本必须写在同一条命令里，进程会随 shell 一起被回收）：
    taskkill /F /IM fastnote.exe; target-app/debug/fastnote.exe sample-vault/设计.md & \
      sleep 8; python tools/ui_check.py

覆盖 8 个浮层 + 默认态 + 工具条浮现 + 左右侧栏，输出 shot_ui_*.png。
"""
import ctypes, ctypes.wintypes as wt, sys, time

sys.path.insert(0, 'tools')
import shot

u = ctypes.windll.user32

VK_CTRL, VK_SHIFT, VK_ESC = 0x11, 0x10, 0x1B
VK_OEM_5 = 0xDC   # '\\'
VK_OEM_COMMA = 0xBC   # ','
VK_OEM_2 = 0xBF   # '/?' —— Ctrl+? 在 Windows 上就是 Ctrl+Shift+/


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


def ensure_front(hwnd):
    """SetForegroundWindow 单独用不可靠（非前台进程调用会被系统忽略），
    先轻按 ALT 解开前台锁，再用截图亮度确认前台确实是 fastnote。"""
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
        p = img.load()
        w, h = img.size
        samples = [p[int(w * 0.6), int(h * 0.5)], p[int(w * 0.5), int(h * 0.75)]]
        bright = all(sum(c) > 3 * 200 for c in samples)
        print(f"  attempt {attempt}: fg={fg == hwnd} bright={bright}")
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
    if not ensure_front(hwnd):
        print("fastnote 不在前台（可能有别的窗口压着），放弃以免误操作")
        return 1

    def move(x, y, wait=1.0):
        u.SetCursorPos(r.left + int(x), r.top + int(y))
        time.sleep(wait)

    def click(x, y, wait=1.0):
        u.SetCursorPos(r.left + int(x), r.top + int(y))
        time.sleep(0.2)
        u.mouse_event(0x0002, 0, 0, 0, 0)
        u.mouse_event(0x0004, 0, 0, 0, 0)
        time.sleep(wait)

    def key(*vk, wait=1.3):
        for k in vk:
            u.keybd_event(k, 0, 0, 0)
        for k in reversed(vk):
            u.keybd_event(k, 0, 2, 0)
        time.sleep(wait)

    def grab(name):
        print(f"{name}:", shot.capture(hwnd, f'shot_ui_{name}.png'))

    # 1) 默认净界面：鼠标停在正文中间 -> 无顶栏、无侧栏
    move(700, 500)
    grab('default')

    # 2) 鼠标贴窗口顶端 -> 工具条浮现
    move(600, 40)
    grab('chrome')

    # 3) 移开 -> 收起
    move(700, 500)
    grab('hidden')

    # 4) 逐个浮层（鼠标先离开顶端，避免工具条盖住面板）
    click(700, 500)  # 先给编辑区焦点，否则按键会发到别的窗口
    overlays = [
        ('palette', (VK_CTRL, 0x4B)),                      # Ctrl+K 命令面板
        # 注意：不能用 Ctrl+, —— Windows 上该键的 WM_KEYDOWN 在 gpui 平台层就被吞掉，
        # 按键根本到不了 app（已用 intercept_keystrokes 探针确认）。代码里真正能被触发的
        # 是 Ctrl+Shift+,（到达形状 key="<"、mods=ctrl，绑定写成 `ctrl-<`）。
        ('settings', (VK_CTRL, VK_SHIFT, VK_OEM_COMMA)),   # Ctrl+Shift+, AI 设置
        ('help', (VK_CTRL, VK_SHIFT, VK_OEM_2)),  # Ctrl+? 快捷键
        ('search', (VK_CTRL, VK_SHIFT, 0x46)),    # Ctrl+Shift+F 搜索
        ('graph', (VK_CTRL, VK_SHIFT, 0x47)),     # Ctrl+Shift+G 图谱
        ('chat', (VK_CTRL, VK_SHIFT, 0x51)),      # Ctrl+Shift+Q 问答
        ('board', (VK_CTRL, VK_SHIFT, 0x42)),     # Ctrl+Shift+B 白板
        ('history', (VK_CTRL, VK_SHIFT, 0x48)),   # Ctrl+Shift+H 历史
    ]
    for name, combo in overlays:
        move(620, 300, wait=0.3)
        key(*combo)
        grab(name)
        key(VK_ESC)

    # 5) 左右侧栏
    click(700, 500)
    key(VK_CTRL, VK_OEM_5)
    grab('sidebar')
    key(VK_CTRL, VK_SHIFT, 0x52)
    grab('panels')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
