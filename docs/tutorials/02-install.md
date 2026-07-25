# 02 — Install crates-music

**Goal:** the crates-music player running on your machine, playing music
from Navidrome in a browser.
**Time:** ~30 minutes, most of it waiting for the build.
**You need:** [guide 01](./01-navidrome.md) finished — a running Navidrome,
its username and password, and your server's network address.

## Before you start: two decisions that are hard to undo

Read this bit properly. Both settings below are **locked in on first
startup**, and changing them afterwards takes extra steps
([troubleshooting](./troubleshooting.md#i-need-to-change-the-hostname-after-first-boot)
covers the fix). Two minutes here saves you an hour later.

**1. What name will you use to reach this?**

Everything works out of the box under the name `gateway.local`, and this
guide uses it. If you already know you want a different name (say your
machine is `nuc.local`, or you own a domain), set it now rather than later.

**2. Which addresses may sign you in?**

The login system will only redirect back to addresses that were registered
at first boot. List every address you might use — including ones you'll
only need later, on your phone. Extra entries are harmless; a missing one
breaks login.

## Step 1 — Get the code

You need the source, because there's no pre-built image to download —
you'll build it locally in step 3.

If you have `git`:

```bash
git clone https://github.com/nicolasrix/crates-music.git
cd crates-music
```

If you don't, download the ZIP from
<https://github.com/nicolasrix/crates-music> (green **Code** button →
**Download ZIP**), unpack it, and `cd` into the folder.

## Step 2 — Write your settings file

Copy the template:

```bash
cp docker/.env.example .env
```

Open the new `.env` file in a text editor. It has a lot of commented-out
options — ignore all of them. Set these three, which are the only required
ones:

```bash
NAVIDROME_URL=http://192.168.1.42:4533
NAVIDROME_USERNAME=your-navidrome-username
NAVIDROME_PASSWORD=your-navidrome-password
```

Use the **network address** you wrote down in guide 01, not `localhost`.
crates-music runs inside a container, and inside that container `localhost`
means the container itself, not your machine — so `localhost` here is the
single most common reason the gateway can't find Navidrome.

Now add the sign-in addresses, on one line, comma-separated, no spaces:

```bash
OAUTH_WEB_REDIRECT_URIS=https://gateway.local:8443/oauth/callback,https://192.168.1.42:8443/oauth/callback
```

Substitute your own address for `192.168.1.42`. Listing both means you can
reach the player by name *or* by raw address later without breaking login.

> **Where's the password stored?** In plain text in `.env`, and in the
> generated config inside the container. This is a home-network tool and
> that file is readable by anyone with access to this machine — so don't
> reuse an important password for your Navidrome account.

## Step 3 — Build and start

```bash
docker compose up --build
```

Note there's no `-d` this time — we want to watch the output, because the
next step needs something printed here.

**This takes a while.** It compiles the whole Rust project from source:
10–20 minutes on a modern laptop or desktop, considerably longer on a small
machine. It wants a few GB of free RAM and about 5 GB of disk. Later starts
take seconds; only this first build is slow.

Two containers start: `crates-gateway` (the player) and `crates-embedder`
(a placeholder for the recommendation engine — see
[guide 04](./04-recommendations.md)).

## Step 4 — Grab the setup code

Once the build finishes, look through the output for a line like this:

```
WARN gateway is unconfigured — visit https://0.0.0.0:8443/oauth/setup with token: 4f3c9a2b1e...
```

**Copy that long token.** It's shown once, and it's the proof that you're
the person who owns this server.

> **Ignore the URL in that message.** `0.0.0.0` is not a real address you
> can visit, and that page is not something a browser can open — the next
> step is the actual way to use the token. (The message is misleading; it's
> a known wart.)

Leave this terminal running and open a **second** terminal window for the
next step.

## Step 5 — Set your password

In the new terminal, run this — substituting your token and choosing your
own password:

```bash
curl -k -X POST https://localhost:8443/oauth/setup \
  -d "token=PASTE_THE_LONG_TOKEN_HERE" \
  -d "password=choose-a-long-password-here"
```

Requirements and notes:

- The password must be **at least 12 characters**. Shorter is rejected.
- This is the master password for the whole system. It is not recoverable
  by email — there is no email. Put it in a password manager now.
- `-k` tells `curl` not to worry about the certificate warning. That's safe
  here because you're talking to your own machine over `localhost`, and
  [the certificates explainer](../explainers/certificates.md) says why the
  warning exists at all.

Success looks like **no output at all**, or a bare `OK`. That's it — the
command finished silently because it worked.

If you get `410 Gone`, a password is already set (you've done this before);
skip ahead. If you get `403`, the token was copied wrong — check for a
missing character at either end.

## Step 6 — Sign in

Open <https://localhost:8443> in a browser.

**Your browser will show a scary warning** — "Your connection is not
private," "Warning: Potential Security Risk Ahead," or similar. This is
expected and it is not a sign that anything is wrong.

Click **Advanced**, then **Proceed to localhost (unsafe)** (Chrome/Edge) or
**Accept the Risk and Continue** (Firefox). In Safari, click **Show
Details** → **visit this website**.

If you want to know exactly what you're agreeing to before you click —
which is a reasonable thing to want — read
[why the security warnings?](../explainers/certificates.md). The short
version: the connection *is* encrypted, but your browser can't verify who's
on the other end, because you're your own certificate authority here.

At the login page:

- **Username:** `owner` (or leave it blank — blank means the owner account)
- **Password:** the one you set in step 5

You should land on the album list, showing your Navidrome library. Click an
album, click a track, and it should play.

**That's the install done.**

## Step 7 — Make it run in the background

Go back to the first terminal and press `Ctrl+C` to stop the stack. Then
start it properly:

```bash
docker compose up -d
```

Now it runs in the background and restarts automatically when the machine
reboots. Useful commands from this folder:

| What | Command |
|---|---|
| See what's running | `docker compose ps` |
| Watch the logs | `docker compose logs -f` |
| Stop everything | `docker compose down` |
| Start again | `docker compose up -d` |
| Update after `git pull` | `docker compose up -d --build` |

## What you have now

- ✅ A web player at `https://localhost:8443` on this machine
- ✅ Your whole Navidrome library, browsable and playable
- ✅ One admin account (`owner`)
- ⚠️ Reachable only from *this* machine so far — [guide 03](./03-phone.md)
  fixes that
- ⚠️ Recommendations produce nonsense until you do
  [guide 04](./04-recommendations.md) (the placeholder engine returns random
  results by design)

## Where things are stored

Everything persists in a Docker "volume" called `crates-music_gw-data`,
which survives rebuilds and updates. It holds your account and password,
the certificate, cached album art, downloaded audio, and any
recommendation data.

The one genuinely irreplaceable piece is the account database. To back it
up:

```bash
docker run --rm -v crates-music_gw-data:/data -v "$PWD:/backup" \
  alpine tar czf /backup/crates-backup.tar.gz -C /data state
```

That writes `crates-backup.tar.gz` into the current folder. The full
backup-and-restore procedure, including how to restore it, is in
[DEPLOYMENT.md](../DEPLOYMENT.md#backups).

## Next

- [03 — Put it on your phone](./03-phone.md) — the good part
- [04 — Turn on real recommendations](./04-recommendations.md)
- [05 — Add family and guests](./05-household.md)
- [Something's broken](./troubleshooting.md)
