"""Generate Comparator Studio's raster icons with Pillow."""
from pathlib import Path

from PIL import Image, ImageDraw

assets = Path(__file__).resolve().parent / "assets"
master = Image.new("RGBA", (1024, 1024))
draw = ImageDraw.Draw(master)
draw.rounded_rectangle((56, 56, 968, 968), radius=211, fill="#1e594b")

# Offset folder silhouettes read clearly in the dock and at window-icon size.
back = [(399, 286), (524, 286), (565, 326), (759, 326), (759, 589), (399, 589)]
front = [(251, 450), (379, 450), (421, 490), (613, 490), (613, 751), (251, 751)]
draw.polygon(back, fill="#92d4b9")
draw.line(front + [front[0]], width=58, fill="#1e594b", joint="curve")
draw.polygon(front, fill="#f5fffa")

icon = master.resize((64, 64), Image.Resampling.LANCZOS)
icon.save(assets / "comparator.png")
(assets / "comparator.rgba").write_bytes(icon.tobytes())
master.resize((512, 512), Image.Resampling.LANCZOS).save(assets / "comparator-512.png")
master.save(assets / "comparator.ico", sizes=[(n, n) for n in (16, 24, 32, 48, 64, 128, 256)])
master.save(assets / "comparator.icns")
