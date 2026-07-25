# 01 — Get your music library online

**Goal:** a running Navidrome that knows about your music files.
**Time:** ~20 minutes, plus scanning time for large libraries.
**Skip this if:** Navidrome is already running somewhere on your network.
Jump to [02 — Install crates-music](./02-install.md) and have its address
and login ready.

## Why Navidrome first

crates-music has no music library of its own. It asks Navidrome "what
albums exist?" and "send me this song." Navidrome is the part that reads
your files, reads the artist and album tags inside them, and serves the
audio.

This split is deliberate: Navidrome is a mature, well-maintained project
with thousands of users, and there's no reason to reimplement it. See
[how it all fits together](../explainers/how-it-works.md) if you want the
longer version.

Navidrome is a separate project with its own documentation. This guide gets
you to "good enough for crates-music" — for anything beyond that, the
[official Navidrome docs](https://www.navidrome.org/docs/) are the real
reference.

## Step 1 — Install Docker

Docker is a tool that runs pre-packaged software without you having to
install its dependencies by hand. Both Navidrome and crates-music ship as
Docker containers, so this is a one-time cost that pays for itself twice.

- **Windows or Mac:** install
  [Docker Desktop](https://www.docker.com/products/docker-desktop/). It's a
  normal application installer. Launch it once and leave it running.
- **Linux:** follow
  [Docker's install guide](https://docs.docker.com/engine/install/) for your
  distribution, then
  [the post-install step](https://docs.docker.com/engine/install/linux-postinstall/)
  that lets you run Docker without typing `sudo` every time.

Check it worked. Open a terminal and run:

```bash
docker --version
```

You should see something like `Docker version 27.3.1, build ce1223035a`. The
exact numbers don't matter. If instead you see "command not found," Docker
isn't installed or isn't on your PATH — restart the terminal first, since
installers often don't update an already-open window.

## Step 2 — Decide where your music lives

Find the folder containing your music files and write down its full path.
Examples:

- macOS: `/Users/yourname/Music/library`
- Windows: `C:\Users\yourname\Music`
- Linux: `/home/yourname/Music`

Navidrome reads tags embedded in the files (artist, album, track number),
not folder names. If your files are badly tagged, Navidrome will faithfully
show you a mess. Tools like [MusicBrainz Picard](https://picard.musicbrainz.org/)
fix that, and it's much less painful to do before you import than after.

Navidrome only ever **reads** this folder in the setup below — the `:ro` in
the configuration means read-only, so it cannot modify or delete your music.

## Step 3 — Create the Navidrome configuration

Make a folder to hold Navidrome's own data (its database, not your music):

```bash
mkdir -p ~/navidrome/data
cd ~/navidrome
```

Now create a file called `docker-compose.yml` in that folder. Use any text
editor — TextEdit, Notepad, VS Code, whatever you have. Paste this in:

```yaml
services:
  navidrome:
    image: deluan/navidrome:latest
    container_name: navidrome
    ports:
      - "4533:4533"
    restart: unless-stopped
    environment:
      ND_LOGLEVEL: info
      # Rescan for new files every hour.
      ND_SCANSCHEDULE: 1h
    volumes:
      - "./data:/data"
      # ⬇️ CHANGE THIS. Left of the colon is YOUR music folder.
      - "/Users/yourname/Music:/music:ro"
```

**Change the last line** to your music folder from step 2. Keep the
`:/music:ro` part exactly as it is — that's the path *inside* the container,
and Navidrome expects it.

> **Windows paths:** use forward slashes and quote the whole thing, e.g.
> `- "C:/Users/yourname/Music:/music:ro"`.

## Step 4 — Start it

From the `~/navidrome` folder:

```bash
docker compose up -d
```

`-d` means "detached" — it runs in the background and gives you your
terminal back. The first run downloads the Navidrome image, which takes a
minute or two.

Watch it start up:

```bash
docker compose logs -f
```

You'll see it scanning your library. Press `Ctrl+C` to stop watching (this
does **not** stop Navidrome — it keeps running in the background).

A first scan of a large library takes a while: roughly a minute per few
thousand tracks, longer on a Raspberry Pi or if your music is on a network
drive. You can continue to the next step while it works.

## Step 5 — Create your account

Open <http://localhost:4533> in a browser.

Navidrome asks you to create an admin account on first visit. **Write down
the username and password** — crates-music needs both in the next guide,
and there's no email-based recovery.

You should now see your albums. Play something to confirm audio works.

> **If the library is empty:** the scan may still be running (check
> `docker compose logs -f` again), or the path in step 3 is wrong. Run
> `docker compose exec navidrome ls /music` — if that prints nothing, the
> folder didn't get mounted, and the left-hand side of that `volumes` line
> is the thing to fix.

## Step 6 — Find your server's network address

crates-music needs to reach Navidrome, and later your phone needs to reach
crates-music. Both need this machine's address on your home network.

```bash
# macOS
ipconfig getifaddr en0

# Linux
hostname -I | awk '{print $1}'

# Windows (PowerShell)
(Get-NetIPAddress -AddressFamily IPv4 | Where-Object { $_.InterfaceAlias -notmatch 'Loopback' }).IPAddress
```

You'll get something like `192.168.1.42`. Addresses starting with `192.168.`,
`10.`, or `172.16`–`172.31` are private home-network addresses — that's what
you want.

**Write this down.** You'll need it in the next guide.

> This address can change when your router restarts. If music stops working
> weeks later for no apparent reason, this is the first suspect. Setting a
> "DHCP reservation" or "static lease" in your router's settings pins it
> permanently, and is worth the five minutes.

## You're done

You now have:

- ✅ Navidrome running at `http://<your-ip>:4533`
- ✅ An admin username and password written down
- ✅ Your server's network address written down

Next: [02 — Install crates-music](./02-install.md).
