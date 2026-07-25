# crates Design System

**crates** is a self-hosted music player web app for a Navidrome backend, served via a custom Rust gateway. Single user, local-network-first, designed for a vinyl-digger / collector mindset (the name comes from "crate digging").

This design system describes the visual language for the React web client (and is forward-compatible with the planned Compose Multiplatform Android app — token names are platform-neutral).

---

## Sources reviewed

- **GitHub repo:** `nicolasrix/crates-music` (default branch `main`)
- **Imported into this project for reference:**
  - `apps/web/index.html`
  - `apps/web/tailwind.config.js` — current Tailwind setup (stone palette, system-ui)
  - `apps/web/src/index.css` — Tailwind directives only
  - `apps/web/src/components/Layout.tsx` — sticky header, max-w-5xl, stone-950 bg
  - `apps/web/src/components/AlbumCard.tsx`
  - `apps/web/src/pages/Albums.tsx` — newest 60 grid (2/3/4/5 cols responsive)
  - `apps/web/src/pages/Album.tsx` — cover + tracklist with double-click-to-play
  - `apps/web/src/pages/SignIn.tsx`
  - `apps/web/src/pages/Diagnostics.tsx` — data-dense traces, histogram, RUM tables
  - `apps/web/src/player/PlayerBar.tsx` — bottom-fixed player
  - `apps/web/src/player/PlayerContext.tsx` — sync-driven audio shell
- **Architecture notes:** `CLAUDE.md` from the repo (Rust gateway, OAuth 2.1, recommender, sync, diagnostics)

> The current web UI is intentionally minimal — Tailwind stone palette, system-ui, plain `<audio>`. This design system is the next step: a real visual identity that keeps that engineering restraint but earns visual character from each album's artwork.

---

## Index

| File | Purpose |
|---|---|
| `README.md` | Brand context, content fundamentals, visual foundations, iconography |
| `SKILL.md` | Skill manifest for use in Claude Code or as a downloadable agent skill |
| `colors_and_type.css` | All design tokens: color, type, spacing, radius, shadow, motion |
| `fonts/` | Local fonts (where applicable) |
| `assets/` | Logos, icons, illustrative artwork samples |
| `preview/` | One HTML card per token cluster — these are what the Design System tab renders |
| `ui_kits/web/` | Pixel-faithful React/Tailwind recreations of the web app surface |
| `ui_kits/web/index.html` | Click-thru prototype: sidebar + albums + album detail + player + diagnostics |

---

## Brand at a glance

> **crates** — a self-hosted music player for the people who still alphabetize their records.

- **Audience:** an audience of one (you). Built for the owner-operator who runs their own Navidrome, doesn't want a subscription, and reads stack traces for fun.
- **Voice:** lowercase, plainspoken, slightly engineer-y. Confident without selling.
- **Aesthetic anchors:** record sleeves, library-card catalogs, terminal UIs, the inside of an audio interface. *Not* Spotify. *Not* iOS skeumorphism.
- **Color philosophy:** the chrome is neutral and quiet so the artwork can drive each page's mood. Every album/artist/playlist page extracts a dominant palette from the cover and tints the surrounding surface — the design system gives that mechanism the rails it needs.

---

## CONTENT FUNDAMENTALS

**Voice:** lowercase, calm, technically precise. Engineer talking to engineer. No marketing frosting.

**Casing:** `lowercase` for navigation, page titles, button labels, and section headers. `Sentence case` for body copy and dialogs that need clarity. Track / album / artist names render in **whatever case the metadata says** — never normalize.

**Pronouns:** prefer no pronoun at all. Where unavoidable, **"your"** (your library, your queue) over "my" — this is the user's data on their hardware.

**Tone of microcopy:**

| Surface | Example (do) | Avoid |
|---|---|---|
| Empty state | `no albums yet — point the gateway at a Navidrome and refresh.` | "Looks like there's nothing here! 🎵" |
| Error | `error: gateway returned 503. retry?` | "Oops! Something went wrong." |
| Loading | `loading…` | "Hold tight while we get your music ready" |
| Settings label | `audio cache budget` | "How much music to keep offline" |
| CTA | `sign in` / `play album` / `pin track` | "Get started" / "Listen now" |

**Numbers and units:** always show units, prefer SI / canonical forms. `2 GB`, `120 ms`, `48 kHz`, `320 kbps`. Durations render `m:ss` or `h:mm:ss` — never `2 minutes 15 seconds`.

**Emoji:** none. Not in UI, not in copy. The product has no emoji budget.

**Truncation:** `…` (real ellipsis), never `...`. Track titles truncate from the right with `text-overflow: ellipsis`.

**Vibe:** *"a single quiet shelf with everything labeled correctly."*

---

## VISUAL FOUNDATIONS

### Color

Two systems running side by side:

1. **Chrome palette** — neutral, dark-first, used for sidebar, header, player, tables, diagnostics. Built from an OKLCH-spaced ink scale (`ink-50 … ink-950`) plus a single warm accent (`crate-amber`, evoking aged paper and tape labels).
2. **Artwork palette** — extracted at runtime from the page's primary cover art and exposed via four CSS custom properties: `--art-bg`, `--art-fg`, `--art-mute`, `--art-accent`. Detail pages set these on `<main>`; the chrome stays neutral so contrast never drifts. When unset, they fall back to the chrome neutrals.

**Semantic colors** (`--success`, `--warning`, `--danger`, `--info`) are tuned to read well against both the dark and light chrome, and against any artwork tint. They are used **only for state** — never decoration.

**Light mode:** same OKLCH ramp inverted with adjusted lightness anchors (not just `invert()`). Chrome is warm-paper white (`#FAF8F4`), not pure white.

### Type

Three typefaces, one job each:

| Family | Use | Source |
|---|---|---|
| **Söhne** ➜ falls back to **Inter** | UI / body / nav | Google Fonts (Inter) — *flagged substitution; ship Söhne if licensed* |
| **Bodoni Moda** (ATF Bodoni revival) | Page titles, album/artist hero names | Google Fonts — variable optical-size 6..96 |
| **JetBrains Mono** | Track durations, codec/bitrate, IDs, diagnostics tables | Google Fonts |

> ⚠️ **Font substitution to confirm:** Söhne is commercial; the system ships with Inter as a visually similar Google Font. If you have a Söhne license, drop the WOFF2 files in `fonts/` and update `colors_and_type.css`. **Bodoni Moda** is the chosen display face — it's an ATF Bodoni revival with classic record-sleeve / Didone proportions and a full optical-size axis.

The display face is reserved for the **album / artist / playlist title on detail pages**, set large (clamp 40–80px) with optical-size enabled. Everything else is the UI sans.

Numbers in tables, durations, and timecodes are always **tabular-nums**.

### Spacing & rhythm

8px base. Token scale: `space-0 (0)`, `space-1 (4)`, `space-2 (8)`, `space-3 (12)`, `space-4 (16)`, `space-5 (24)`, `space-6 (32)`, `space-7 (48)`, `space-8 (64)`, `space-9 (96)`.

Vertical rhythm in dense surfaces (album track table, diagnostics) is **6px** (`--row-y`) — tight but legible.

### Backgrounds

- **No gradients on chrome.** Surfaces are flat solids.
- The *only* place gradient appears: the soft top-of-page wash on detail pages, a 320px-tall vertical fade from `--art-bg` (alpha 0.6 → 0) into the chrome. This is the visual seam between artwork tint and neutral chrome.
- **No patterns, no textures, no decorative SVG.** The cover art is the imagery.
- Full-bleed cover-art hero on artist and playlist pages: blurred (`filter: blur(60px) saturate(1.3)`), 30% opacity, behind a `--ink-950 / 0.7` veil.

### Borders & dividers

- Hairlines, always 1px, always `--border-subtle` (`oklch(28% 0.005 80 / 1)` dark, `oklch(88% 0.005 80 / 1)` light). No double borders. No 2px borders.
- Tables: bottom-border on `<thead>`, hairline between rows. Never full-grid.
- Cards: **no border**. Cards are defined by elevation (a lighter background), not a stroke.

### Corner radii

| Token | Value | Use |
|---|---|---|
| `--radius-0` | 0 | Tables, sidebar dividers |
| `--radius-1` | 4px | Tags, pills, small inline buttons |
| `--radius-2` | 6px | Buttons, inputs, menu items |
| `--radius-3` | 10px | Cards, album cover thumbnails |
| `--radius-4` | 16px | Large hero covers, modals |
| `--radius-full` | 9999px | Avatars, the play button on the player bar, scrub thumb |

**Note:** album/cover thumbnails use `--radius-3` (slightly rounded — vinyl sleeves don't have sharp corners under stage lights). The Play button on the player bar uses `--radius-2` (a soft rounded square), matching the rest of the chrome — there are no fully-circular UI elements.

### Shadows / elevation

Two-shadow rule. Every elevated surface uses **one** of these — never custom values.

| Token | Use |
|---|---|
| `--shadow-pop` | Hover-raised album cards, dropdowns, the active scrub thumb |
| `--shadow-overlay` | The persistent player bar (top-edge inner shadow, no outer) and modals |

Light mode shadows are softer and slightly warm. Dark mode shadows are deeper and slightly cool.

### Animation & motion

- **Easing:** custom `--ease-out-soft` (`cubic-bezier(0.2, 0.7, 0.2, 1)`) for entries, `--ease-in-soft` for exits. No bounce, no overshoot.
- **Durations:** `--dur-1 (90ms)` for hover/active, `--dur-2 (180ms)` for menu/popover, `--dur-3 (320ms)` for page-level fades. Anything > 320ms is wrong.
- **Page transitions:** crossfade only. No slide, no slide-up sheet (we are not iOS).
- **The scrubber:** the only place with an *intentional* live animation. The progress fill animates linearly while playing.
- **Reduced motion:** `prefers-reduced-motion: reduce` collapses all transitions to 0ms except the scrubber, which becomes step-wise (1s ticks).

### Hover & press states

| State | Treatment |
|---|---|
| Hover (clickable text / nav) | color shifts from `--fg-muted` → `--fg` (no underline) |
| Hover (button, primary) | background lightens by 4% L* in OKLCH |
| Hover (button, ghost / secondary) | background goes from transparent → `--surface-2` |
| Hover (track row) | background `--surface-2`, play/pause glyph appears in the track-number cell |
| Press / active | background darkens by 4% L*, no shrink, no scale transform |
| Focus-visible | 2px ring of `--focus` (= `--art-accent` if set, else `--ink-200`), 2px offset |

### Transparency & blur

Used **only** where the chrome must layer over artwork:

- Sticky top header: `bg-[--surface-0] / 0.72` + `backdrop-blur(14px)`
- Player bar: `bg-[--surface-0] / 0.85` + `backdrop-blur(20px)` + 1px top hairline
- Sidebar over a tinted page: never blurs — the sidebar is opaque.

If `backdrop-filter` is unsupported, surfaces fall back to opaque (no aesthetic loss).

### Cards

A "card" in crates is **a thumbnail with a label below it**, no border, no shadow at rest, and `--shadow-pop` on hover. The album/artist/playlist tile is the canonical card. Avoid bordered + rounded + shadowed "panel" cards — those are anti-pattern here.

### Layout rules

- Desktop-first. Primary breakpoint at **`lg: 1024px`**; sidebar collapses below to a bottom nav. Phone target: **360px wide**.
- Sidebar: fixed-width **240px** on desktop, full-bleed sheet on mobile.
- Player bar: **fixed bottom**, 80px tall on desktop (72px on mobile), spans full viewport width above the sidebar (`z-index: 50` over everything).
- Content max-width: **1280px**. List grids cap at 6 cols at 1280+.
- Track table on album page is **full-width**, no max.

### Density & data tables (Diagnostics)

The diagnostics surface is intentionally dense. It uses:

- 13px base, 11px headers (uppercase, letter-spacing 0.04em).
- 6px row padding.
- `--mono` family throughout numeric columns.
- The histogram inline bar uses three layered fills (p50 emerald, p95 amber, p99 red) at 30–60% alpha — matches the existing `Diagnostics.tsx`.

---

## ICONOGRAPHY

The repo ships **no custom icon set or icon font**. The current UI uses Unicode glyphs for player controls (`‹`, `›`) — that's the floor we're building above.

**Decision:** use **Lucide** (https://lucide.dev) at **1.5px stroke**, served via the `lucide-static` SVG sprite. Lucide is MIT-licensed, has every glyph the player needs (play, pause, skip-forward, skip-back, repeat, shuffle, search, plus, list-music, disc-3, mic-2, library, sliders-horizontal, more-horizontal, x), and matches the system's "quiet engineering" tone better than Heroicons or Material.

> ⚠️ **Substitution flag:** Lucide is not in the original repo. It is the closest CDN match for the implied stroke style and is what `ui_kits/web/` uses. If the user has a preferred icon set, swap it.

**Rules of use:**

- Default size: **18px**. Player-bar primary controls: **22px**. Sidebar nav: **18px**. Inline metadata: **14px**.
- Stroke: 1.5px (Lucide default is 2px — override via `stroke-width="1.5"`).
- Color: inherits `currentColor`. Never colored fill unless the icon is itself a state indicator (e.g. the like-heart filled `--danger`).
- Spacing: **8px gap** between icon and label inside buttons. Icon-only buttons get a 36×36 hit target minimum (44×44 on touch).
- The primary **Play** button on the player bar is a soft rounded square (`--radius-2`), filled with the active artwork accent, icon white, 40×40 (player bar) / 56×56 (album hero).

**No emoji. No PNG icons. No custom hand-drawn SVGs in chrome.**

Album / artist / playlist artwork is *not* iconography — it's the photographic content of the product, and is treated under the cover-art rules in Visual Foundations.
