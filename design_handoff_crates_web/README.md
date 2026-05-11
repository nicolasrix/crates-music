# Handoff: crates web client

## Overview

This bundle contains the **crates** design system and a click-through HTML prototype of the web client. crates is a self-hosted music player for a Navidrome backend, served via a custom Rust gateway — single user, local-network-first, designed for the owner-operator who runs their own server.

The current production codebase (`apps/web` in `nicolasrix/crates-music`) is React + Vite + Tailwind 4 with a stone palette and system-ui — intentionally minimal. This design system is the next step: a real visual identity that earns visual character from each album's artwork while keeping the engineering restraint.

## About the design files

The files in this bundle are **design references created in HTML / inline JSX** — prototypes showing intended look and behavior, not production code to copy directly. Your task is to **recreate these designs in the existing `apps/web` codebase** (React + TypeScript + Tailwind 4 + react-router + react-query + a Subsonic API client), using its established patterns. The design tokens in `colors_and_type.css` are framework-neutral CSS custom properties — they are meant to be lifted into a Tailwind 4 `@theme { ... }` block (or an equivalent CSS file imported once at the app root) and then *referenced*, not copy-pasted into every component.

## Fidelity

**High-fidelity.** Final colors, typography, spacing, motion, and interactions. Recreate pixel-faithfully. Match measurements exactly; use the existing Tailwind / CVA / class-variance-authority pattern in the repo for variants.

## What's in this bundle

| Path | Purpose |
|---|---|
| `SYSTEM.md` | The design system itself: brand context, content fundamentals, visual foundations, iconography. **Read this first.** |
| `colors_and_type.css` | Every design token: ink scale, surfaces, semantic colors, type scale, spacing, radius, shadow, motion. Lift into Tailwind 4 `@theme`. |
| `prototype/` | Click-through React prototype (inline JSX). Open `prototype/index.html` to interact with it. **Source of truth for layout, interactions, and per-component styling.** |
| `preview/` | One HTML card per token cluster (type, color, spacing, components, brand). Use as the visual reference for any single token. |
| `assets/` | Logo mark, wordmark, cover-placeholder SVG. |

## The central mechanism: artwork-tinted detail pages

This is the most important rule in the system and the hardest one to retrofit, so do this first:

1. The chrome (sidebar, header, player bar, body background) is **always neutral** — it reads from `--surface-0`, `--fg`, `--fg-muted`, etc.
2. Every detail page (album, artist, playlist) sets four CSS variables on its `<main>` element from a palette extracted from the cover art:
   - `--art-bg` — page wash background
   - `--art-fg` — hero title color
   - `--art-mute` — secondary text on the hero
   - `--art-accent` — scrubber fill, now-playing row tint, hero play button, focus ring
3. Components that should tint per-page (`.scrub-bar .fill`, `.now-playing td`, `.play-disc`, `.tinted-wash`, the focus ring) read from `var(--art-accent)` with a fallback to `var(--accent)`.
4. The neutral chrome **never** reads from `--art-*` — it stays consistent across pages.

In production, extract the palette using a worker (e.g. `node-vibrant` server-side at sync time, or `extract-colors` client-side on first paint of a detail page) and cache the four hex values per album_id. Don't extract on every render.

## Screens

The prototype implements seven screens. Each is a function in `prototype/Pages.jsx`.

### 1. Albums (`/`, `route.page = "albums"`)
- **Top bar:** sticky, 56px tall, hairline bottom border, back/forward arrows, breadcrumb ("albums"), settings icon right-aligned.
- **Section header:** `<h2>Newest</h2>` + count "N of 60" right-aligned + lead text "refreshed from the gateway 4 minutes ago." in `--fg-muted`.
- **Grid:** `grid-template-columns: repeat(auto-fill, minmax(160px, 1fr))`, gap `var(--space-5)` (24px). Each tile is a `Cover` (aspect-ratio: 1, `--radius-3`) + title + sub. **No card border, no shadow at rest.** On hover: `transform: translateY(-2px)`, `--shadow-pop`, and a 44×44 amber play-overlay button slides in from `bottom-12px right-12px` (opacity 0 → 1, translateY 8px → 0).
- **Click:** entire tile → album detail. The play-overlay button's onClick is stopped so it can play directly without navigating.

### 2. Album detail (`/album/:id`)
- Sets `--art-*` on `<main>` from the album's palette.
- **Tinted wash:** `<div class="tinted-wash">` absolutely positioned 0/0/auto/0, height 320px, `linear-gradient(to bottom, color-mix(in oklab, var(--art-bg) 60%, transparent) 0%, transparent 100%)`.
- **Hero:** flex row, gap 32px, align-items: flex-end, padding 48px. 240×240 cover with `--shadow-pop` and `--radius-3`. Right side: kind label ("album" uppercase), Bodoni Moda title at clamp(40px, 6vw, 80px), sub-row with artist · year · track count · duration · `Tag` chips for FLAC / 48 kHz, then an actions row: a 56×56 `--radius-2` filled-amber `.play-disc` + secondary icon buttons (add to queue, more).
- **Tracklist:** full-width `<table class="tracks">` with sticky `<thead>`, hairline row dividers, hover highlights row + swaps the number cell for a play glyph. The currently-playing row tints in `--art-accent` and replaces the number with an animated 3-bar equalizer.
- **Double-click a row** → play that track (current behavior in `Album.tsx` to preserve).

### 3. Tracks (all-tracks)
- Same `.tracks` table but flat across the library — adds an "album" column linking to the album.

### 4. Artists (grid)
- Same tile grid as Albums but covers are circular (`border-radius: 50%`) with a radial-gradient placeholder (no real artist photos in the kit).

### 5. Artist detail
- Same as Album detail except: cover is a circle, blurb paragraph below the sub-row, and the section below is the artist's albums (tile grid).

### 6. Playlist
- Same as Album detail; cover is a 2×2 quilt of the contained albums' covers (use CSS grid `1fr 1fr / 1fr 1fr` with `overflow:hidden` on a `--radius-3` box).

### 7. Diagnostics
- Data-dense. Mirrors the schema in `apps/web/src/api/diagnostics.ts` exactly.
- Stat tiles row (`.tile-stat`): 4 stats — not started / in progress (warning-colored if > 0) / done / failed (danger-colored if > 0).
- Span histogram table with an inline 3-segment box-bar (p99 danger@30%α, p95 warning@35%α, p50 success@50%α layered, plus a 1px max marker). `--font-mono`, 13px.
- RUM events table with a `Pill` rendering the rating in good / needs-improvement / poor tones.

## Persistent UI

### Sidebar (`prototype/Sidebar.jsx`)
- Fixed-width **240px**, `--surface-1` background, hairline right border.
- Brand row at top: 28px disc-mark SVG + "crates" wordmark in Bodoni Moda 22px.
- Search input.
- `nav-group` labels (uppercase, `--type-label`, `--fg-faint`).
- `nav-item` rows: 7×10 padding, `--radius-1`, color `--fg-muted` → `--fg` on hover with `--surface-2` bg. Active state: same hover bg + a 2px amber rule offset 6px from each end on the left.
- Playlists list shows a small accent-colored square swatch + count right-aligned in `--font-mono`.

### Player bar (`prototype/PlayerBar.jsx`)
- Fixed bottom, **80px** tall (72px on mobile), `z-index: 50`, full viewport width above the sidebar.
- Background: `color-mix(in oklab, var(--surface-0) 88%, transparent)` + `backdrop-filter: blur(20px)` + 1px top hairline.
- Three-column grid: now-playing (cover 52×52 `--radius-2` + title/sub), transport+scrubber column, autoplay toggle right-aligned.
- Transport buttons: 36×36 `--radius-2` ghost icons, plus a 40×40 `--radius-2` filled play/pause button (foreground = `--fg`, icon = `--surface-0`).
- Scrubber: 4px-tall track in `--surface-3`, fill in `var(--art-accent, var(--fg))`, 12px circular thumb visible on hover only.
- Autoplay toggle: pill with a 6px dot, on-state uses `color-mix(in oklab, var(--accent) 14%, transparent)` background + `--accent` foreground.
- Reads the active track's `art.accent` and writes `--art-accent` on the player wrapper so the scrubber tints to match.

## Tokens (lift into Tailwind 4 `@theme`)

All the source of truth is in `colors_and_type.css`. The most-referenced ones:

| Token | Value (dark, default) |
|---|---|
| `--surface-0` | `oklch(14% 0.005 70)` (ink-50) |
| `--surface-1` | `oklch(18% 0.005 70)` (ink-100) |
| `--surface-2` | `oklch(22% 0.006 70)` (ink-200) |
| `--surface-3` | `oklch(28% 0.006 70)` (ink-300) |
| `--fg` | `oklch(95% 0.006 70)` (ink-900) |
| `--fg-muted` | `oklch(78% 0.008 70)` (ink-700) |
| `--fg-faint` | `oklch(52% 0.008 70)` (ink-500) |
| `--accent` | `oklch(72% 0.13 65)` (crate-amber-500) |
| `--border-subtle` | `oklch(28% 0.005 80 / 1)` |
| `--success` / `--warning` / `--danger` / `--info` | see file |
| `--radius-1..4 / -full` | 4 / 6 / 10 / 16 / 9999 |
| `--space-1..9` | 4 / 8 / 12 / 16 / 24 / 32 / 48 / 64 / 96 |
| `--shadow-pop`, `--shadow-overlay` | only two shadows allowed |
| `--dur-1/2/3` | 90 / 180 / 320 ms |
| `--ease-out-soft` | `cubic-bezier(0.2, 0.7, 0.2, 1)` — no overshoot |

Light mode: same names redefined inside `[data-theme="light"]` (warm-paper white #FAF8F4 surface, inverted ink ramp).

## Type

| Family | Source | Use |
|---|---|---|
| **Inter** (fallback for Söhne) | Google Fonts | All UI / body / nav |
| **Bodoni Moda** (variable opsz 6..96, ATF Bodoni revival) | Google Fonts | Page titles, album/artist hero names — *display only* |
| **JetBrains Mono** | Google Fonts | Track durations, codecs, IDs, diagnostics tables, all numeric columns (`font-variant-numeric: tabular-nums`) |

If you have a Söhne license, drop the WOFF2 in `apps/web/public/fonts/` and update `--font-sans`. The display family is intentionally classic-record-sleeve (Bodoni); don't substitute a humanist or geometric.

## Iconography

**Lucide** (https://lucide.dev, MIT) at **1.5px stroke**. The repo doesn't currently use it — install `lucide-react`. Default size 18px; player-bar primary 22px; sidebar nav 18px; inline metadata 14px. The prototype's `Pieces.jsx` traces the exact glyphs needed inline — use those names as the canonical set.

**No emoji. No PNG icons. No custom hand-drawn SVGs in chrome.**

## Voice

- **lowercase** for nav, page titles, button labels, section headers.
- **Sentence case** for body and dialog copy.
- **Track / album / artist metadata renders verbatim** — never normalize case.
- Numbers always with units: `2 GB`, `120 ms`, `48 kHz`, `320 kbps`.
- Durations: `m:ss` or `h:mm:ss`. Never "2 minutes 15 seconds".
- "your library" / "your queue" — avoid "my" and "our".
- Real ellipsis `…`, never `...`.
- Empty / error microcopy is engineer-direct: `"error: gateway returned 503. retry?"` not `"Oops!"`.

## State management

The prototype manages player state in a single `useState` in `app.jsx` (`{ track, queue, queueIndex, isPlaying, position, repeat, autoplay, onPrev, onNext }`). In the real codebase this lives in `PlayerContext.tsx` (existing) and is sync-driven — keep that. The design adds:
- `autoplay` boolean (recommends + plays next when queue ends — wire into the recommender API)
- `art` palette per track (cached from album sync; falls back to neutral if absent)

## Motion

Three durations only: 90 / 180 / 320 ms. One ease for entries (`--ease-out-soft`), one for exits. **No bounce, no overshoot.** Page transitions are crossfade only — no slide. The scrubber is the one place with intentional live animation; under `prefers-reduced-motion: reduce` it ticks step-wise once per second instead.

## Files in this bundle

```
SYSTEM.md                  — design system reference (read first)
colors_and_type.css        — all tokens
prototype/
  index.html               — open this in a browser to interact
  app.jsx                  — root, route state, sample data wiring
  Sidebar.jsx              — left nav
  PlayerBar.jsx            — bottom-fixed transport
  Pages.jsx                — all 7 screens
  Pieces.jsx               — Icon, Cover, IconBtn, Pill, Tag, Search primitives
  data.jsx                 — sample albums / tracks / playlists / diagnostics rows
  kit.css                  — prototype-specific styles (composes with ../../colors_and_type.css)
  README.md                — prototype-internal notes
preview/                   — one HTML card per token cluster
assets/
  logo-mark.svg            — disc with center label
  logo-wordmark.svg        — Bodoni "crates"
  cover-placeholder.svg    — generic cover slot
```

## Implementation order (suggested)

1. Lift `colors_and_type.css` tokens into a Tailwind 4 `@theme` block. Verify dark/light mode swap.
2. Add Bodoni Moda + JetBrains Mono to `index.html` font links. Confirm Söhne fallback chain.
3. Install `lucide-react`. Replace existing Unicode glyphs in `PlayerBar.tsx`.
4. Build the chrome shell (sidebar + topbar + player bar) against neutral tokens only.
5. Wire the artwork-palette extraction (worker side, cached per album_id) and apply `--art-*` on detail-page mounts.
6. Rebuild Albums + Album pages against the new tokens — these have the most visual weight.
7. Rebuild Diagnostics with the histogram box-bar (this is the one data-viz piece, don't reach for a charting lib).
8. Artists / Playlist screens.
9. Light-mode QA.
