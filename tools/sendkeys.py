"""枚举按键 -> app 实际收到的 keystroke，自动判定「被吞掉」的按键。

用途：排查「某个快捷键完全没反应」。它区分两种完全不同的病因 ——
  (a) 按键根本没送到 app（平台层/IME 吞了，日志里连一行都没有）；
  (b) 按键送到了但没匹配上任何绑定（日志里有该按键，但 action=<none>）。

做法：每个测试前先按一个**唯一的 F 键**当标记（F1、F2…），F 键在 gpui 里走
`parse_immutable` 一定送达、且本应用没绑动作，所以日志里出现 fN 就等于
"第 N 个测试开始了"；如果 fN 的下一条就是 f(N+1)，说明第 N 个测试的按键
**整个没送到 app**。

坑（踩过）：
  * **别用 F10 当标记** —— 会进入 Windows 的窗口菜单循环，之后所有按键都收不到，
    日志直接断在 f10。用 F1~F9，且每个测试用不同的 F 键。
  * 测试数量别超过 12（F 键只用 F1~F12）。

依赖：需要 app 侧临时挂一个 `cx.intercept_keystrokes` 探针把 keystroke 写进
`%TEMP%/fastnote_keyprobe.log`（格式见 main.rs 里那段已删除的 TEMP-PROBE，
需要时按原样加回 Workspace::new）。

读 %TEMP%/fastnote_keyprobe.log。
    taskkill /F /IM fastnote.exe; target-app/debug/fastnote.exe & \
      sleep 9; python tools/sendkeys.py
"""
import ctypes, ctypes.wintypes as wt, os, sys, time

u = ctypes.windll.user32
VK_CTRL, VK_SHIFT, VK_ALT, VK_ESC = 0x11, 0x10, 0x12, 0x1B
VK_F1 = 0x70
VK_K, VK_BACKSLASH, VK_COMMA, VK_SLASH, VK_PERIOD = 0x4B, 0xDC, 0xBC, 0xBF, 0xBE
VK_BRACKET, VK_BRACKET_R, VK_SEMI, VK_QUOTE, VK_MINUS, VK_EQUAL, VK_TILDE = (
    0xDB, 0xDD, 0xBA, 0xDE, 0xBD, 0xBB, 0xC0)

TESTS = [
    ('裸 ,', (VK_COMMA,)),
    ('Ctrl+,', (VK_CTRL, VK_COMMA)),
    ('Ctrl+Shift+,', (VK_CTRL, VK_SHIFT, VK_COMMA)),
    ('Ctrl+# 再来一次', (VK_CTRL, VK_COMMA)),
    ('裸 `', (VK_TILDE,)),
    ('Ctrl+`', (VK_CTRL, VK_TILDE)),
    ('裸 .', (VK_PERIOD,)),
    ('Ctrl+.', (VK_CTRL, VK_PERIOD)),
    ('Ctrl+Shift+.', (VK_CTRL, VK_SHIFT, VK_PERIOD)),
    ('裸 /', (VK_SLASH,)),
    ('Ctrl+/', (VK_CTRL, VK_SLASH)),
    ('Ctrl+Shift+/', (VK_CTRL, VK_SHIFT, VK_SLASH)),
    ('Ctrl+\\', (VK_CTRL, VK_BACKSLASH)),
    ('Ctrl+;', (VK_CTRL, VK_SEMI)),
    ("Ctrl+'", (VK_CTRL, VK_QUOTE)),
    ('Ctrl+-', (VK_CTRL, VK_MINUS)),
    ('Ctrl+=', (VK_CTRL, VK_EQUAL)),
    ('Ctrl+[', (VK_CTRL, VK_BRACKET)),
    ('Ctrl+]', (VK_CTRL, VK_BRACKET_R)),
]

LOG = os.path.join(os.environ.get('TEMP', r'C:\Windows\Temp'), 'fastnote_keyprobe.log')


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


def main():
    if len(TESTS) > 12:
        print(f'注意：用了 {len(TESTS)} 个测试，F 键标记只能到 F12，超出部分会冲突')

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

    u.ShowWindow(hwnd, 9)
    u.keybd_event(VK_ALT, 0, 0, 0)
    u.keybd_event(VK_ALT, 0, 2, 0)
    u.BringWindowToTop(hwnd)
    u.SetForegroundWindow(hwnd)
    time.sleep(1.2)

    u.SetCursorPos(cx0 + cw // 2, cy0 + ch // 2)
    time.sleep(0.3)
    u.mouse_event(0x0002, 0, 0, 0, 0)
    u.mouse_event(0x0004, 0, 0, 0, 0)
    time.sleep(1.0)

    def press(vks, wait=0.7):
        for vk in vks:
            u.keybd_event(vk, u.MapVirtualKeyW(vk, 0), 0, 0)
        time.sleep(0.12)
        for vk in reversed(vks):
            u.keybd_event(vk, u.MapVirtualKeyW(vk, 0), 2, 0)
        time.sleep(wait)

    press((VK_ESC,), wait=0.6)
    for i, (label, vks) in enumerate(TESTS):
        press((VK_F1 + i,), wait=0.6)   # 唯一标记
        press(vks, wait=0.6)

    # ---- 解析 ----
    try:
        with open(LOG, 'r', encoding='utf-8') as f:
            lines = [l.strip() for l in f if l.strip()]
    except FileNotFoundError:
        print(f'找不到探针日志 {LOG}')
        return 1

    def key_of(line):
        # key="<把变量名换掉>" 里的原始键名
        if not line.startswith('key='):
            return None
        return line[5:line.index(' mods=')]

    print(f'\n日志共 {len(lines)} 行\n')
    print(f'{"测试按键":<18}{"app 收到的":<42}判定')
    print('-' * 76)

    swallowed = []
    idx = 0
    for i, (label, _) in enumerate(TESTS):
        marker = f'"{chr(VK_F1 + i).lower()}"'  # F1 -> "f1" 由 gpui 命名，见下
        # gpui 把 F1 命名为 "f1"（小写），日志里是 key="f1"
        marker_key = f'"f{1 + i}"'
        while idx < len(lines) and key_of(lines[idx]) != marker_key:
            idx += 1
        idx += 1  # 跳过标记本身
        # 标记之后的第一条非标记行就是本测试的按键
        got = None
        while idx < len(lines):
            k = key_of(lines[idx])
            if k is not None and k.startswith('"f') and k[2:3].isdigit():
                break  # 撞到下一个标记 -> 本测试没有按键送达
            got = lines[idx]
            break
        if got is None:
            print(f'{label:<18}{"—":<42}被吞掉 (按键没送到 app)')
            swallowed.append(label)
        else:
            print(f'{label:<18}{got:<42}收到')

    print('\n被吞掉的按键：', swallowed if swallowed else '无')
    return 0


if __name__ == '__main__':
    code = main()
    os.system('taskkill /F /IM fastnote.exe >/dev/null 2>&1')
    raise SystemExit(code)
