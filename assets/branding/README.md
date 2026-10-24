# Truck Launcher branding

Created for the ETS2 / ATS launcher using the built-in `image_gen` tool.
No API-key or CLI fallback was used.

- `logo.png`: original generated master, 1254 × 1254, with transparent outer corners.
- `icon.png`: 256 × 256 PNG used by the native window and application header.
- `icon.ico`: 32-bit alpha icon with 16, 20, 24, 32, 40, 48, 64, 128 and 256 px frames.

To regenerate the icon files from the master on Windows:

```powershell
.\scripts\make-icons.ps1
```

`build.rs` embeds the ICO frames as Win32 `RT_ICON` and `RT_GROUP_ICON`
resources in the executable. This uses standard binary `.res` data and does
not require a separate resource compiler. MSVC link.exe and LLVM LLD are
supported; the project's GNU toolchain wrapper already uses LLD.

## Generation prompt

```text
Use case: logo-brand. Asset type: production logo and Windows app icon for an existing native truck-game launcher supporting Euro Truck Simulator 2 and American Truck Simulator. Create one polished square app icon, 1024 by 1024. A bold, original, geometric front-facing semi-truck emblem, with its lower grille subtly suggesting a download arrow and a road's central lane through negative space. The truck should be a neutral symbolic truck, suitable for both European and American trucks, no manufacturer identity. Match the existing app palette: luminous mint green #68E0A4 for the emblem on a very dark ink #101821 rounded-square tile. Minimal, precise, flat vector-like graphic with substantial solid shapes, clean edges and high contrast, legible at 16 and 32 pixels. Center the mark with balanced 12 percent breathing room. The rounded dark tile occupies most of the canvas; real transparent pixels outside its rounded corners. No text, no letters, no wordmark, no tiny details, no gradients, no glow, no drop shadow, no mockup, no border, no watermark. Show the final icon alone, straight-on, filling the square composition.
```
