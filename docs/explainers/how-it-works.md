# How it all fits together

A plain-language tour of what you installed. No code, and nothing here is
required to *use* the thing — this is for when you're curious why there are
several moving parts instead of one.

For the technical version, see [ARCHITECTURE.md](../ARCHITECTURE.md).

## The pieces

```
   your music files
          │
   ┌──────▼──────┐
   │  Navidrome  │   reads your files, knows your library
   └──────┬──────┘
          │
   ┌──────▼───────────────┐
   │  crates-music        │   accounts, recommendations, sync,
   │  (the "gateway")     │   caching, offline coordination
   └──┬────────────────┬──┘
      │                │
 ┌────▼─────┐   ┌──────▼──────────┐
 │ terminal │   │  web player     │
 │   app    │   │  = phone app    │
 └──────────┘   └─────────────────┘
```

### Navidrome — your library

Scans your music folder, reads the tags inside each file, and builds a
catalogue. It knows what you own; it serves the audio. It's a separate,
mature project this one deliberately doesn't try to replace.

### crates-music — the gateway

Sits between Navidrome and everything else. "Gateway" because everything
passes through it. It handles what Navidrome doesn't:

- **Accounts and roles** — Navidrome has one login here; this splits it
  into separate accounts with separate taste and playlists
- **Recommendations** — analysing how tracks actually sound
- **Sync** — pause on your phone, resume on your laptop
- **Caching** — remembering things so the app feels instant
- **Offline** — coordinating what gets downloaded to a device

### The clients

The **web player** is the main one, and it's also the phone app — installed
to your home screen, it's the same code with no browser frame. There's no
separate mobile app to maintain, which is why the phone experience improves
whenever the web one does.

The **terminal app** is a full player for people who live in a terminal.
Same server, same accounts.

## What happens when you press play

Roughly, in order:

1. The player asks the gateway for the track.
2. The gateway checks whether it already has it. If yes, it sends it
   straight back.
3. If not, it fetches from Navidrome, passes it along, and remembers it.
4. Meanwhile the gateway has already worked out what you'll probably play
   next, and quietly starts fetching the beginning of it.

That last step is why the next track usually starts instantly. The system
guesses ahead and pre-loads.

## Why so much caching

The design target is that the app responds to a tap in under a twentieth of
a second, and starts playing in under a fifth. You can't hit that if every
tap waits for a network round trip — so the strategy is to never ask the
network for something already known.

Four layers, each a fallback for the one before:

| Layer | What it holds | Where |
|---|---|---|
| Screen data | Whatever you're looking at | Memory |
| Library info | Album and artist details | Small database on each device |
| Audio | Songs you've played | Disk |
| Downloads | Songs you explicitly saved | Disk, protected |

The distinction in the last two rows matters in practice. Ordinary
listening fills the third layer, and it's cleaned out oldest-first as space
runs low. **Saved-for-offline** tracks live in the fourth, with their own
budget, and are never automatically deleted. That's why an album you saved
for a flight is still there weeks later, while something you played once
isn't.

## How recommendations work

Genre tags are unreliable and don't capture much. So instead, the system
listens.

Every track's first couple of minutes goes through a model that converts
sound into a long list of numbers — a summary of its texture, instruments,
rhythm, and tone. Similar-sounding music produces similar numbers.
"More like this" is then just "find the nearest numbers."

Two useful consequences:

**Tags don't matter.** Two tracks filed under different genres that
genuinely sound alike will be found. Badly-tagged libraries still work.

**Words map into the same space.** The model was trained on descriptions
alongside audio, so a phrase like "melancholy piano" becomes numbers in the
*same* space as the music. Typing a description and finding matching tracks
is the same nearest-neighbour search, with a sentence as the starting point
rather than a song.

This works better for concrete musical language — instruments, tempo,
texture, genre — than for abstract or emotional phrasing. "Distorted
guitars, fast drums" gets you much closer than "music for a rainy day."
That's a limitation of the model, and a known weak spot.

The system also watches what you skip, replay, and rate, and tilts results
accordingly. Guests' listening is deliberately excluded so visitors don't
reshape your taste.

## Accounts and privacy

One Navidrome account sits behind everything, so the **library is shared** —
everyone sees the same albums.

Everything else is **per person**: queue, history, likes, playlists,
recommendations, settings. Your partner's late-night listening doesn't
affect your recommendations.

Guests are a special case, built for having people over. A guest joins
*your* room — they control what's playing on the speakers rather than
getting a private session. They can't save playlists or rate things, their
access expires, and their listening never trains anything.

## Why is it built this way?

**Why not just use Navidrome's own web player?** You could — it's decent.
This exists for the things it doesn't do: sound-based recommendations,
proper offline on a phone, cross-device sync, per-person taste.

**Why a gateway in the middle instead of clients talking to Navidrome
directly?** Anything worth sharing between devices has to live somewhere
central. Recommendations need the whole library's analysis. Sync needs one
place that decides what "the queue" is. A cache is only useful if it's
shared. Putting that in each client would mean building it three times and
having them disagree.

**Why is the phone app just a website?** Because a native app would need a
second codebase, an app store, and a build pipeline, to deliver features
the web platform already provides for a self-hosted LAN player: home-screen
install, offline playback, background audio, lock-screen controls. There
was a native Android app planned; it was dropped once it became clear the
web version covered it.

**Why does the analysis run separately from everything else?** It's the
only part that wants serious hardware. Keeping it separate means it can run
on a different machine — a gaming PC doing the heavy work while a small
always-on box serves the music — and the player keeps working normally when
it's off, just with weaker recommendations.

## Related

- [Guide 01 — Get your music library online](../tutorials/01-navidrome.md)
- [Why the security warnings?](./certificates.md)
- [Glossary](./glossary.md)
- [ARCHITECTURE.md](../ARCHITECTURE.md) — the technical version
