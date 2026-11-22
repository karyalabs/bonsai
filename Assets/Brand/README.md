# Bonsai brand

| File | Use |
| --- | --- |
| `bonsai-mark.svg`, `bonsai-mark-dark.svg` | The mark on its own, for light and dark backgrounds (the Welcome screen uses these) |
| `bonsai-icon.svg`, `bonsai-icon-dark.svg` | The mark on its rounded tile (favicons, avatars, small sizes) |
| `bonsai-icon-macos.svg`, `bonsai-icon-macos-dark.svg` | The macOS app icon: tile, padding and shadow on a 1024 canvas |
| `*.png` | Renders of the above |

The app icon itself is `../AppIcon/Bonsai.icon` (Icon Composer: one layer per
shape, with light and dark colors in `icon.json`) and `../AppIcon/Bonsai.icns`
(the fallback for macOS before 26, rendered from `bonsai-icon-macos.svg`).

## Colors

| Shape | Light | Dark |
| --- | --- | --- |
| Lower pad | `#1FA463` | `#2BBF78` |
| Upper pad | `#A6DB4A` | `#B4E95C` |
| Overlap | `#0B4F36` | `#E6FFB8` |
| Trunk | `#3B2A20` | `#B08A69` |
| Pot | `#E2694A` | `#F07A58` |
| Tile | `#FFFFFF` to `#EDEDF0` | `#26282E` to `#1B1C20` |
