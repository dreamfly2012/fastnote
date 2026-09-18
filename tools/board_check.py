"""白板实跑验证：打开白板 -> 加节点 -> 拖动 -> 连线 -> 截图 + 回读 md 校验。

画布位置由截图里量出来（扫最长的一条近边框色 run，长度落在 875~886 才算画布，
排除更宽的面板外框），标题栏按钮则用实测常量 —— 窗口 1196x839 下这三个值稳定，
窗口尺寸或面板内边距变了要重新量。

用法：先 taskkill 掉旧进程并启动 release，再跑本脚本（必须同一条命令，
进程会随启动它的 shell 一起被回收）。落盘状态要先清空，否则节点数不对、
推算出的节点坐标全是错的。
"""
import ctypes, ctypes.wintypes as wt, sys, time
from PIL import Image

sys.path.insert(0, 'tools')
import shot

u = ctypes.windll.user32
VK_SHIFT = 0x10
CANVAS_W, CANVAS_H = 880.0, 520.0

# 布局常量：窗口 1196x839 下从截图实测（画布白底区域 + 标题栏按钮的边框位置）。
# 窗口尺寸一致时这几个值就稳定，改布局要重新量。
CANVAS_LEFT, CANVAS_TOP = 159.5, 159.5
HEADER_Y = 122.0
# 实测的三个标题栏按钮中心：+ 节点 / 连线 / 删除
ADD_X, LINK_X, DEL_X = 875.0, 931.5, 981.5


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


def is_border(c):
    r, g, b = c
    return abs(r - 215) < 16 and abs(g - 218) < 16 and abs(b - 224) < 16


def find_canvas(path):
    """找画布边框（长 ~880 的水平线），返回 (left, top, bottom)。"""
    img = Image.open(path).convert('RGB')
    px = img.load()
    W, H = img.size
    rows = []
    for y in range(60, H - 30):
        run = best = 0
        cur = bestx = 0
        for x in range(W):
            if is_border(px[x, y]):
                if run == 0:
                    cur = x
                run += 1
                if run > best:
                    best, bestx = run, cur
            else:
                run = 0
        if 875 < best < 886:   # 只认画布自己的边框（880 宽），排除面板外框（904）
            rows.append((y, bestx, best))
    if len(rows) < 2:
        return None
    return rows[0][1], rows[0][0], rows[-1][0]


def find_buttons(path, y):
    """在 y 这一行找按钮的左右竖边，返回 [(中心x, 左, 右)]。"""
    img = Image.open(path).convert('RGB')
    px = img.load()
    W, _ = img.size
    xs = [x for x in range(700, W - 20) if is_border(px[x, y])]
    groups = []
    for x in xs:
        if groups and x - groups[-1][-1] <= 2:
            groups[-1].append(x)
        else:
            groups.append([x])
    edges = [g[0] for g in groups]
    out = []
    for i in range(0, len(edges) - 1, 2):
        out.append(((edges[i] + edges[i + 1]) // 2, edges[i], edges[i + 1]))
    return out


def main():
    hwnd = find_window()
    if not hwnd:
        print("no window")
        return 1
    r = wt.RECT()
    u.GetWindowRect(hwnd, ctypes.byref(r))
    print("window", r.left, r.top, r.right - r.left, r.bottom - r.top)
    u.SetForegroundWindow(hwnd)
    time.sleep(0.4)

    def click(x, y, wait=0.9):
        u.SetCursorPos(r.left + int(x), r.top + int(y))
        time.sleep(0.2)
        u.mouse_event(0x0002, 0, 0, 0, 0)
        u.mouse_event(0x0004, 0, 0, 0, 0)
        time.sleep(wait)

    def drag(x1, y1, x2, y2, steps=10):
        u.SetCursorPos(r.left + int(x1), r.top + int(y1))
        time.sleep(0.25)
        u.mouse_event(0x0002, 0, 0, 0, 0)
        time.sleep(0.25)
        for i in range(1, steps + 1):
            u.SetCursorPos(
                r.left + int(x1 + (x2 - x1) * i / steps),
                r.top + int(y1 + (y2 - y1) * i / steps),
            )
            time.sleep(0.07)
        u.mouse_event(0x0004, 0, 0, 0, 0)
        time.sleep(0.8)

    # 1) 顶栏「白板」按钮
    # 首次点击可能只用于激活窗口、不传给应用，先点正文把焦点拿稳
    click(700, 620, wait=0.6)

    # 顶栏「白板」：打不开就再点一次
    btns = []
    for attempt in (1, 2):
        click(572, 39, wait=1.6)
        shot.capture(hwnd, 'shot_board_empty.png')
        btns = find_buttons('shot_board_empty.png', int(HEADER_Y))
        if len(btns) == 3:
            break
        print(f"第 {attempt} 次点「白板」没打开，重试")
    print("header buttons:", btns)
    if len(btns) != 3:
        print("白板浮层没打开，放弃")
        return 1

    cl, ct = CANVAS_LEFT, CANVAS_TOP
    print("canvas left/top:", cl, ct)

    click(ADD_X, HEADER_Y)          # + 节点：够两个节点就能验拖动与连线
    click(ADD_X, HEADER_Y)
    print("nodes:", shot.capture(hwnd, 'shot_board_nodes.png'))

    # 节点落点由 core 的 add_node 公式决定：0.3+col*0.1 / 0.25+row*0.16
    def node_center(col, row):
        return (cl + (0.3 + col * 0.1) * CANVAS_W, ct + (0.25 + row * 0.16) * CANVAS_H)

    x1, y1 = node_center(0, 0)
    x2, y2 = node_center(1, 0)

    # 2) 拖动：第一个节点往右下挪
    drag(x1, y1, x1 + 130, y1 + 150)
    print("dragged:", shot.capture(hwnd, 'shot_board_drag.png'))

    # 3) 连线：选中源节点 -> 「连线」按钮 -> 点目标节点
    click(x1 + 130, y1 + 150, wait=0.6)
    click(LINK_X, HEADER_Y, wait=0.6)
    click(x2, y2, wait=1.2)
    print("linked:", shot.capture(hwnd, 'shot_board_link.png'))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
