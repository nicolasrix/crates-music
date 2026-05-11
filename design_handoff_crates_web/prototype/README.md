# crates web — UI kit

A pixel-faithful, click-through recreation of the **crates** web client.

## What's here

- `index.html` — interactive prototype: sidebar + albums grid + album detail + tracks list + artist page + playlist page + diagnostics + persistent player
- `app.jsx` — root, route state, sample data
- `Sidebar.jsx` — fixed-width left nav with playlists
- `PlayerBar.jsx` — bottom-fixed transport (play, pause, prev/next, scrubber, repeat, autoplay toggle)
- `Pages.jsx` — Albums, Album, Tracks, Artists, Artist, Playlist, Diagnostics screens
- `Pieces.jsx` — small primitives: Tile, TrackRow, IconBtn, Pill, Tag, Search, Cover, Icons (Lucide-traced SVGs)

## Design rules being demonstrated

- Chrome stays neutral (`--surface-0` / `ink-50`).
- Each detail page sets `--art-bg`, `--art-fg`, `--art-mute`, `--art-accent` from a sampled cover. The scrubber, the now-playing-row tint, the hero gradient wash, and the focus ring all read from those variables.
- Type: Inter UI, Fraunces display for hero titles only, JetBrains Mono for numbers/IDs/diagnostics.
- Lucide icons at 1.5px stroke, 18/22/14 sizes.
- No emoji. No PNG icons. No bordered shadowed "panel" cards.
- All measurements use the tokens defined in `../../colors_and_type.css`.

## What's not here (UI kit, not a real app)

- No real audio. The player updates state and animates the scrubber, but never streams anything.
- No real auth, no real Subsonic calls. The data is a hand-curated array of jazz/funk classics with cover gradients and synthetic playcounts.
- The diagnostics screen renders against a baked sample that mirrors the schema in `apps/web/src/api/diagnostics.ts`.

## How to extend

The components are deliberately small and single-purpose. To add a new page, write a function in `Pages.jsx` returning JSX, route to it from the sidebar in `app.jsx`. To add a new component state, edit the relevant primitive in `Pieces.jsx` — they all consume CSS vars from `../../colors_and_type.css`, so theme/tint changes happen in one place.
