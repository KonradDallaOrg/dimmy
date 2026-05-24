# Icon assets

The Dimmy app icon (cloud + waveform, mint→blue gradient) is **not**
authored here. The single source of truth is the brand kit at
`~/Pictures/dimmy-brand/` (master, kept out of the repo). This folder
holds only the rendered artefacts the Windows app actually loads.

## What lives here

| File | Source in brand kit | Consumed by |
|---|---|---|
| `dimmy.ico` | `windows/dimmy.ico` (16–256 multi-size) | EXE icon, taskbar, Alt-Tab, **system tray** (`TrayService`), **Velopack installer** (`--icon` in release workflows), jump-list default |
| `dimmy-logo.png` | `windows/icon-512.png` (512², gradient, transparent) | Settings → About logo (120×120 display) |

The tray intentionally reuses the full-gradient `dimmy.ico` (the tray
background on Win11 is dark, so the gradient reads fine). The brand kit
also ships dedicated monochrome tray icons (`tray/tray-template-*` black,
`tray/tray-light-*` white) if a more subdued native look is ever wanted —
wire `TrayService.LoadTrayIcon` to those instead.

## Updating the icon

1. Edit the design in the brand kit, run its `generate.py`.
2. Copy `windows/dimmy.ico` → `dimmy.ico` and `windows/icon-512.png`
   → `dimmy-logo.png` here.
3. Rebuild. Windows aggressively caches shell icons — if the taskbar
   still shows the old icon after a rebuild, it's the OS icon cache, not
   a stale binary (the in-app About logo updates immediately).

The macOS equivalents live in
`platforms/macos/Dimmy/Assets.xcassets/AppIcon.appiconset/` (7 PNGs,
16–1024) + `DimmyLogo.imageset/`, sourced from the brand kit's
`macos/Dimmy.iconset/`. Linux uses `platforms/linux/assets/`.
