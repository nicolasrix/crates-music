# 05 — Add family and guests

**Goal:** everyone in the house gets their own account, and visitors can
join without one.
**Time:** ~10 minutes.
**Optional** — skip it if you're the only listener.

## The idea

Everyone shares one music library but gets their own everything else. Your
partner's queue, their liked songs, their playlists, and their
recommendations are entirely separate from yours. Nobody's taste pollutes
anybody else's.

Three kinds of account:

| Role | Can do | Typical use |
|---|---|---|
| **Admin** | Everything, plus manage accounts and server settings | You |
| **User** | Play, rate, make playlists, download — all private to them | Household members |
| **Guest** | Play, and control whatever's playing in the room they joined | Visitors |

Guests are deliberately limited: they can't create playlists or rate
things, their accounts expire on their own, and — importantly — what they
listen to is **not** used to train your recommendations. A friend playing
their favourite album all evening won't reshape what the system thinks you
like.

## Adding a household member

1. Sign in as `owner` (or any admin).
2. Open **Settings → account**. Admins get a user-management section at the
   bottom of that page; non-admins don't see it at all.
3. Add a user. Provide:
   - **Username** — what they'll type to sign in
   - **Password** — at least 12 characters
   - **Role** — pick **user** unless they should administer the server
4. Tell them the username, password, and the address (e.g.
   `https://gateway.local:8443`).

They sign in with **their own username**, not `owner`. This matters: the
username field is what separates their data from yours.

They can change their own password later in **Settings → account**.

> **There is no email anywhere in this system**, so there's no "forgot
> password" link. If someone forgets theirs, an admin resets it from
> **Settings → account**. That's the whole recovery mechanism, and it's why
> the [owner password recovery](#if-you-forget-the-owner-password) section
> below exists.

### If you're already signed in and want to test

Signing out in the app doesn't always fully clear the server-side session,
so clicking "sign in" can put you straight back into your own account. Use
the **"sign in as a different user"** link on the sign-in page, which
forces a fresh prompt. A private/incognito window works too.

## Inviting a guest

Guests don't need an account. You hand them a code.

1. **Settings → guests**.
2. Create a guest code. You get a short code to share.
3. The visitor opens the player's address and redeems the code instead of
   signing in.

What a guest gets:

- Your library, browsable and playable
- Control over the shared queue in **your room** — this is the party
  jukebox case. They add something, it plays on the speakers
- Your recommendations, read-only

What a guest can't do: create playlists, rate anything, change settings, or
influence future recommendations. Their access expires automatically, and
you can revoke a code any time from the same screen — existing sessions
using it keep working until they expire, but nobody new can join with it.

> Guest codes are shared secrets. Anyone with a code and a route to your
> network can listen to your library, so hand them out with roughly the
> care you'd give your Wi-Fi password.

## What stays private, and what doesn't

Worth being clear about, since people ask.

**Private to each account:** play queue, playback position, liked and
disliked tracks, playlists, recommendations, listening history, downloads,
and settings.

**Shared by everyone:** the music library itself — albums, artists, tracks,
and cover art. There's one Navidrome account behind the scenes, so the
catalogue is common ground.

**Visible to admins:** admins can create and delete accounts and reset
passwords. They can also see server diagnostics, which include recent
activity. An admin is not prevented from seeing what's been played. This is
a household tool with a household trust model — treat "admin" as
"co-owner", not as a neutral role.

## If you forget the owner password

There's no email recovery, so the fallback is proving you control the
server itself. On the machine running it:

```bash
docker compose exec gateway sh -c \
  'printf "%s" "your-new-password" | music-gateway --config /data/config/gateway.toml reset-master-password'
```

This rewrites the owner's password and preserves everything else — the
account, playlists, history, and other users are untouched.

For other people's accounts, an admin resets them from **Settings → account**
instead; this command is only for the owner account.

## Next

- [Something's broken](./troubleshooting.md)
- [How it all fits together](../explainers/how-it-works.md)
