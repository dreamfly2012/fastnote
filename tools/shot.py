"""Expand a window then capture it, using Windows APIs via ctypes."""
import ctypes, ctypes.wintypes as wt, struct, zlib, sys

u = ctypes.windll.user32
g = ctypes.windll.gdi32

SW_RESTORE = 9
SW_SHOWDEFAULT = 10
SRCCOPY = 0x00CC0020
DIB_RGB_COLORS = 0
BI_RGB = 0


class BITMAPINFOHEADER(ctypes.Structure):
    _fields_ = [
        ("biSize", wt.DWORD), ("biWidth", wt.LONG), ("biHeight", wt.LONG),
        ("biPlanes", wt.WORD), ("biBitCount", wt.WORD), ("biCompression", wt.DWORD),
        ("biSizeImage", wt.DWORD), ("biXPelsPerMeter", wt.LONG),
        ("biYPelsPerMeter", wt.LONG), ("biClrUsed", wt.DWORD), ("biClrImportant", wt.DWORD),
    ]


def png_encode(rgb_rows, w, h):
    raw = bytearray()
    for row in rgb_rows:
        raw.append(0)
        raw += row

    def chunk(tag, data):
        c = struct.pack(">I", len(data)) + tag + data
        return c + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)

    ihdr = struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)
    out = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr)
    out += chunk(b"IDAT", zlib.compress(bytes(raw), 6))
    out += chunk(b"IEND", b"")
    return out


def capture(hwnd, path):
    u.ShowWindow(hwnd, SW_RESTORE)
    u.SetForegroundWindow(hwnd)
    ctypes.windll.kernel32.Sleep(1200)

    r = wt.RECT()
    u.GetWindowRect(hwnd, ctypes.byref(r))
    x, y = r.left, r.top
    w, h = r.right - r.left, r.bottom - r.top
    if w <= 0 or h <= 0:
        raise SystemExit(f"bad rect {w}x{h}")

    hdc_screen = u.GetDC(0)
    hdc_mem = g.CreateCompatibleDC(hdc_screen)
    hbmp = g.CreateCompatibleBitmap(hdc_screen, w, h)
    g.SelectObject(hdc_mem, hbmp)
    g.BitBlt(hdc_mem, 0, 0, w, h, hdc_screen, x, y, SRCCOPY)

    bi = BITMAPINFOHEADER()
    bi.biSize = ctypes.sizeof(BITMAPINFOHEADER)
    bi.biWidth = w
    bi.biHeight = -h
    bi.biPlanes = 1
    bi.biBitCount = 32
    bi.biCompression = BI_RGB
    stride = w * 4
    buf = (ctypes.c_ubyte * (stride * h))()
    g.GetDIBits(hdc_mem, hbmp, 0, h, buf, ctypes.byref(bi), DIB_RGB_COLORS)

    rows = []
    mv = bytes(buf)
    for yy in range(h):
        row = mv[yy * stride:(yy + 1) * stride]
        line = bytearray(w * 3)
        line[0::3] = row[2::4]
        line[1::3] = row[1::4]
        line[2::3] = row[0::4]
        rows.append(bytes(line))

    with open(path, "wb") as f:
        f.write(png_encode(rows, w, h))

    g.DeleteObject(hbmp)
    g.DeleteDC(hdc_mem)
    u.ReleaseDC(0, hdc_screen)
    return w, h


def find_hwnd_by_pid(pid):
    EnumWindows = u.EnumWindows
    EnumWindowsProc = ctypes.WINFUNCTYPE(ctypes.c_bool, wt.HWND, wt.LPARAM)
    found = []

    def cb(hwnd, lparam):
        p = wt.DWORD()
        u.GetWindowThreadProcessId(hwnd, ctypes.byref(p))
        if p.value == pid and u.IsWindowVisible(hwnd):
            found.append(hwnd)
            return False
        return True

    EnumWindows(EnumWindowsProc(cb), 0)
    return found[0] if found else 0


if __name__ == "__main__":
    pid = int(sys.argv[1])
    out = sys.argv[2]
    hwnd = find_hwnd_by_pid(pid)
    if not hwnd:
        raise SystemExit("window not found")
    w, h = capture(hwnd, out)
    print(f"captured {w}x{h} -> {out}")
