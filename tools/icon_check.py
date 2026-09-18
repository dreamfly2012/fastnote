"""从 shot_image.png 裁出标题栏图标并与 fastnote.png 并排对比。"""
from PIL import Image

# 标题栏左上角：图标大约在 (10, 6)-(30, 26)
shot = Image.open("shot_image.png")
icon_area = shot.crop((4, 0, 40, 36)).resize((36 * 8, 36 * 8), Image.NEAREST)
icon_area.save("icon_crop.png")

src = Image.open("fastnote.png").convert("RGBA")
# 源图缩到 64 再放到 288，模拟任务栏尺寸观感
small = src.resize((64, 64), Image.LANCZOS).resize((288, 288), Image.NEAREST)
canvas = Image.new("RGB", (288 * 2 + 24, 300), (255, 255, 255))
canvas.paste(small, (8, 6), small)
canvas.save("icon_src.png")
print("ok: icon_crop.png(标题栏放大) / icon_src.png(源图 64px)")
