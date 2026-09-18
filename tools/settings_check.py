"""验证「设置」浮层的快捷键修复：Ctrl+Shift+, 能开、Esc 能关。

    taskkill /F /IM fastnote.exe; target-app/debug/fastnote.exe sample-vault/项目计划.md & \
      sleep 9; python tools/settings_check.py
"""
import ctypes, ctypes.wintypes as wt, os, sys, time

sys.path.insert(0, 'tools')
import shot
from overlay_check import block_diff, find_window

u = ctypes.windll.user32
VK_CTRL, VK_SHIFT, VK_ALT, VK_ESC = 0x11, 0x10, 0x12, 0x1B
VK_COMMA, VK_TILDE, VK_K, VK_BACKSLASH = 0xBC, 0xC0, 0x4B, 0xDC


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
    mid = (cx0 + cw // 2, cy0 + ch // 2)

    u.ShowWindow(hwnd, 9)
    u.keybd_event(VK_ALT, 0, 0, 0)
    u.keybd_event(VK_ALT, 0, 2, 0)
    u.BringWindowToTop(hwnd)
    u.SetForegroundWindow(hwnd)
    time.sleep(1.2)

    def park(wait=0.9):
        u.SetCursorPos(*mid)
        time.sleep(wait)

    def press(vks, wait=1.5):
        for vk in vks:
            u.keybd_event(vk, u.MapVirtualKeyW(vk, 0), 0, 0)
        time.sleep(0.15)
        for vk in reversed(vks):
            u.keybd_event(vk, u.MapVirtualKeyW(vk, 0), 2, 0)
        time.sleep(wait)

    def snap(name):
        park()
        shot.capture(hwnd, name)

    # 先点一下拿焦点
    u.SetCursorPos(*mid)
    time.sleep(0.3)
    u.mouse_event(0x0002, 0, 0, 0, 0)
    u.mouse_event(0x0004, 0, 0, 0, 0)
    time.sleep(1.0)

    snap('st_base.png')

    # 阳性对照：Ctrl+\ 切侧栏，一定会看到明显变化；它是「按键到底有没有送到 app」的判据
    press((VK_CTRL, VK_BACKSLASH))
    snap('st_ctrl.png')
    control = block_diff('st_base.png', 'st_ctrl.png')
    press((VK_CTRL, VK_BACKSLASH))   # 切回去
    snap('st_ctrl2.png')

    press((VK_CTRL, VK_SHIFT, VK_COMMA))
    snap('st_open.png')
    opened = block_diff('st_base.png', 'st_open.png')

    press((VK_ESC,))
    snap('st_esc.png')
    closed = block_diff('st_base.png', 'st_esc.png')

    # 回归：Ctrl+Shift+` 现在应该能切换行内代码（编辑器里，差异较小但要非 0）
    press((VK_CTRL, VK_SHIFT, VK_TILDE))
    snap('st_code.png')
    code = block_diff('st_base.png', 'st_code.png')

    print(f'阳性对照 Ctrl+\\ 切侧栏 : 差异块={control:>6}   {"按键送达 OK" if control > 40 else "按键没送到 app"}')
    print(f'Ctrl+Shift+, 打开设置 : 差异块={opened:>6}   {"OK" if opened > 40 else "NG"}')
    print(f'Esc 关闭            : 差异块={closed:>6}   {"OK" if closed < 40 else "NG"}')
    print(f'Ctrl+Shift+` 行内代码 : 差异块={code:>6}   (参考值，非 0 即生效)')
    ok = control > 40 and opened > 40 and closed < 40
    print('\n结论：', '修复生效' if ok else ('按键没送到，结论无效' if control <= 40 else '仍然不生效'))
    return 0 if ok else 1


if __name__ == '__main__':
    code = main()
    os.system('taskkill /F /IM fastnote.exe >/dev/null 2>&1')
    raise SystemExit(code)
