# Icon source

| | |
| --- | --- |
| Sitter | Leonardo Torres Quevedo (1852–1936) |
| Work | Autochrome portrait, Boulogne, 1 October 1921 |
| Photographer | Auguste Léon, for Albert Kahn's *Archives de la Planète* |
| Holder | Musée départemental Albert-Kahn, Hauts-de-Seine |
| File | [`1921 Leonardo Torres Quevedo.jpg`](https://commons.wikimedia.org/wiki/File:1921_Leonardo_Torres_Quevedo.jpg) on Wikimedia Commons |
| Licence | Public domain |
| Retrieved | 2026-11-11, 1098×1537, 311 128 bytes, SHA-1 `cb9bb53d286b39eceaa39d0713a040f3f39f5234` (matches the value Commons reports) |

`src.jpg` is that file, unmodified. `mask.png` is the subject mask produced from it by
`mask.swift` (Apple Vision, macOS 14 or later) from the `nodalia-brand` repository — the same
tool used for every Nodalia icon. `compose.py` builds the icons from the two.

To regenerate everything in `assets/icons/`:

```bash
python3 design/icon/compose.py assets/icons     # needs Pillow
```

followed by the size exports (PNG sizes, `.ico`, and `.icns` with `iconutil` on macOS).
