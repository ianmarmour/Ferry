# Assets

| File | What it is |
|---|---|
| `ferry.svg` | The app icon |
| `icons/ferry-*.png` | The icon rendered at the sizes the app, tray, desktop entry and macOS app use |
| `ferry.ico` | The icon for Windows executables |
| `ferry-banner.svg` | The README's banner |

After changing `ferry.svg`, render the PNGs with
[resvg](https://github.com/linebender/resvg) and the Windows icon with Pillow:

```bash
for size in 16 22 24 32 48 64 128 256 512; do
  resvg -w $size -h $size assets/ferry.svg assets/icons/ferry-$size.png
done
python3 -c "from PIL import Image; Image.open('assets/icons/ferry-256.png').save('assets/ferry.ico', sizes=[(s, s) for s in (16, 24, 32, 48, 64, 128, 256)])"
```

The banner's text is converted to outlines, so it looks the same everywhere;
regenerate it with `python3 assets/generate-banner.py`, which needs fontTools
and the Inter font.
