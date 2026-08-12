# Wired Terminal brand assets

Source of truth for the **terminal** product mark and app icon.

Sibling products (`wired-slides`, `wired-website`, `wired-book`) share the same
wire-W mark geometry. Differentiation is **plate colour** plus a **top-right
product badge**.

## Mark

A continuous **wire** path that forms a **W**, with terminal and peak nodes —
reads as both the letter and a signal / connection graph. The badge is a
terminal window sitting at a `❯` prompt.

| File | Use |
|------|-----|
| `logo.svg` | Full app icon (plate + mark + terminal badge) — vector |
| `mark.svg` | Mark only, `currentColor` — UI / monochrome |
| `app-icon.png` | 1024×1024 master for `tauri icon` |
| `app-icon-512.png` | Preview / marketing |
| `mark.png` | Transparent mark raster |

## Regenerating platform icons

From `app/`:

```bash
npx tauri icon ../brand/app-icon.png -o src-tauri/icons
```

That refreshes `icon.icns`, `icon.ico`, PNG sizes and store logos used by the
desktop shell (`tauri.conf.json` → `bundle.icon`).

To regenerate `app-icon.png` itself from the vector (macOS, no extra tooling):

```bash
qlmanage -t -s 1024 -o /tmp brand/logo.svg && cp /tmp/logo.svg.png brand/app-icon.png
```

## In-app

React components live in `app/src/components/Brand.tsx`:

- `WiredMark` — monochrome glyph (no badge)
- `WiredAppIcon` — full plate + terminal badge
- `WiredWordmark` — mark + word

`app/public/logo.svg` is the browser favicon and must stay in sync with
`brand/logo.svg`.

## Colours

| Token | Hex | Role |
|-------|-----|------|
| Plate top | `#2e1a5f` | Icon gradient start (violet) |
| Plate bottom | `#0a0718` | Icon gradient end |
| Wire | `#f1ecff` → `#c9b8ff` | Mark on plate |
| Badge chip | `#180d3a` | Top-right product glyph |
| UI accent | `#7c5cff` | App chrome (see `app/src/styles.css`) |

### vs siblings

| Product | Plate | Top-right badge |
|---------|-------|-----------------|
| **Wired Terminal** (this repo) | violet `#2e1a5f` → `#0a0718` | terminal at a prompt |
| **Wired Slide** (`wired-slides`) | blue `#163064` → `#080e22` | stacked slides |
| **Wired Website** (`wired-website`) | teal `#0c5558` → `#041618` | browser window |
| **Wired Book** (`wired-book`) | amber `#5a3518` → `#140c08` | open book |
