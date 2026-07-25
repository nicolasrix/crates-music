# When something goes wrong

Problems grouped by what you're seeing. If yours isn't here, the logs are
the place to look:

```bash
docker compose logs --tail=100 gateway
```

Most failures announce themselves clearly in there.

> **A note on volume names.** Several commands below reference
> `crates-music_gw-data`. Docker builds that name from the folder you ran
> `docker compose` in, so if you renamed the folder, yours differs. Run
> `docker volume ls` and use whichever name ends in `_gw-data`.

---

## Installing

### `docker: command not found`

Docker isn't installed, or your terminal was open before it was installed.
Close the terminal, open a new one, try again. On Mac and Windows, also
check Docker Desktop is actually running — the whale icon should be in your
menu bar or system tray.

### The build fails or gets killed

Usually memory. Compiling this project wants a few GB of free RAM; Docker
kills the build if it runs out. In Docker Desktop, **Settings → Resources**
raises the memory limit — give it 6 GB or more and retry.

On a low-memory machine, close everything else and try again, or build on a
bigger machine.

### `set NAVIDROME_URL in .env`

The `.env` file is missing, empty, or in the wrong folder. It must sit next
to `docker-compose.yml` in the `crates-music` folder, and it's `.env` —
starting with a dot, no extension. Editors on Windows and Mac sometimes
save it as `.env.txt`. Check with `ls -la` (Mac/Linux) or `dir /a`
(Windows).

---

## The gateway won't start or can't reach Navidrome

### It starts, then immediately stops

Read the last few lines of the log — the reason is almost always stated
plainly:

```bash
docker compose logs --tail=30 gateway
```

### Can't connect to Navidrome

Nine times out of ten, `NAVIDROME_URL` is set to `localhost`. Inside a
container, `localhost` refers to that container — not your machine. Use the
network address from [guide 01](./01-navidrome.md#step-6--find-your-servers-network-address):

```bash
NAVIDROME_URL=http://192.168.1.42:4533
```

Check the gateway can actually reach it:

```bash
docker compose exec gateway curl -s "http://192.168.1.42:4533/ping"
```

Any response at all means the network path is fine, and the problem is the
username or password instead.

### Wrong Navidrome username or password

The gateway starts but shows no albums. Confirm the same credentials work
by signing in to Navidrome directly at `http://192.168.1.42:4533`. Fix
`.env`, then `docker compose up -d` to apply.

---

## Signing in

### The setup token doesn't work

- **`403`** — the token was copied wrong. It's long; check you got the
  whole thing, with no trailing space or newline.
- **`410 Gone`** — a password has already been set. The token works exactly
  once. If you don't know the password, use
  [password recovery](#i-forgot-the-owner-password).
- **`400`** — the password is under 12 characters.

### The setup URL in the log doesn't open

Correct — it isn't meant to. The log prints
`https://0.0.0.0:8443/oauth/setup`, which is not a browsable page and not a
reachable address. Use the `curl` command in
[guide 02, step 5](./02-install.md#step-5--set-your-password) instead. The
log message is misleading; it's a known wart.

### Login bounces back to the sign-in page

The address you're using wasn't registered as an allowed sign-in address.
See [changing the hostname](#i-need-to-change-the-hostname-after-first-boot)
below — the fix is the same.

### Signing in puts me back into the wrong account

Signing out clears the app but not always the server-side session. Use the
**"sign in as a different user"** link on the sign-in page, or open a
private/incognito window.

### I forgot the owner password

On the server:

```bash
docker compose exec gateway sh -c \
  'printf "%s" "your-new-password" | music-gateway --config /data/config/gateway.toml reset-master-password'
```

Everything else is preserved. For other people's accounts, an admin resets
them from **Settings → account** instead.

---

## Certificates and browser warnings

### "Your connection is not private"

Expected. Your server issued its own certificate, and no browser trusts
that by default. Click **Advanced → Proceed**. Full explanation:
[why the security warnings?](../explainers/certificates.md).

### My phone won't offer to install the app

The connection isn't fully trusted, and browsers deliberately withhold
app-install and offline features on connections they can't verify. Clicking
through a warning doesn't count. See
[guide 03, part 2](./03-phone.md#part-2--make-your-phone-trust-it).

On Android, also check you're using a Chromium-based browser — Firefox and
DuckDuckGo can't install web apps. On iOS it must be Safari.

### I installed the certificate on my iPhone and it still warns

You almost certainly missed the second step. Installing the profile is not
enough on its own: go to **Settings → General → About → Certificate Trust
Settings** and switch on full trust. This step is easy to miss and required.

### I need to change the hostname after first boot

Both the certificate and the allowed sign-in addresses are fixed on first
startup. Changing `.env` alone won't take — that catches people out, so
here's the whole fix.

```bash
# 1. Stop.
docker compose down

# 2. Set the new name and sign-in addresses in .env:
#    GATEWAY_HOSTNAME=music.local
#    OAUTH_WEB_REDIRECT_URIS=https://music.local:8443/oauth/callback,https://192.168.1.42:8443/oauth/callback

# 3. Delete the certificate so it's reissued for the new name.
docker compose run --rm --entrypoint sh gateway -c \
  'rm -f /data/certs/gateway.local.pem /data/certs/gateway.local-key.pem'

# 4. Delete the registered web client so it re-registers with the new
#    addresses. (It is only ever created if absent, which is why editing
#    .env by itself does nothing.)
docker run --rm -v crates-music_gw-data:/data alpine sh -c \
  "apk add --no-cache sqlite >/dev/null && \
   sqlite3 /data/state/gateway-state.sqlite \"DELETE FROM oauth_clients WHERE client_id='web';\""

# 5. Start.
docker compose up -d
```

Your accounts, playlists, and downloads all survive this. If you'd trusted
the old certificate on any devices, remove it there and trust the new one.

---

## Playback

### Album list loads but nothing plays

Check the gateway can fetch audio from Navidrome — usually the same
credential or address problem as above. `docker compose logs -f gateway`
while pressing play shows the failing request.

### Playback dies after about an hour

A known issue: the session credential expires mid-listen and playback stops
at the next track boundary. Reload the page to recover. A proper fix is
tracked in the project's issues.

### Music stops when my phone screen turns off

Android should keep playing with lock-screen controls. iOS is more
restrictive about background audio for home-screen web apps and is less
reliable here than a native app would be. Confirm the app was opened from
the **home-screen icon** rather than a browser tab — that difference
matters.

---

## Recommendations

### Recommendations are nonsense

If you haven't done [guide 04](./04-recommendations.md), that's expected —
the default engine is a placeholder producing meaningless results by
design.

If you have, check the analysis engine is the real one:

```bash
docker compose logs gateway | grep -i embedder
```

`dim=768` means the real engine. `dim=512` means the placeholder — you're
missing the second `-f docker-compose.clamp3.yml` flag on your
`docker compose` command.

### Text stations return unrelated music

Concrete musical descriptions work considerably better than abstract or
emotional ones. "Distorted guitars, fast drums" beats "music for a rainy
day". This is a limitation of the underlying model, not your setup — see
[guide 04, step 7](./04-recommendations.md#step-7--try-it).

### Analysis seems stuck

Check it's running:

```bash
docker compose logs -f embedder
```

It's genuinely slow — seconds per track, hours for a large library. The
**Settings → ingest** page shows queue depth counting down, which is the clearest
way to confirm progress. If the count isn't moving at all, the engine may
have run out of memory: raise `EMBEDDER_MEM_LIMIT` in `.env` to `8g` and
restart.

---

## Starting over

To wipe **everything** — accounts, settings, downloads, analysis — and
reinstall from scratch:

```bash
docker compose down -v
```

The `-v` deletes the data volume. This cannot be undone. Your music files
and Navidrome are not affected; they're separate.

To reset only the analysis data but keep accounts and settings, see
[guide 04, step 3](./04-recommendations.md#step-3--clear-the-old-index).

---

## Still stuck?

Collect this before asking for help — it answers the first three questions
anyone will have:

```bash
docker compose ps
docker compose logs --tail=50 gateway
docker compose logs --tail=20 embedder
```

Check for passwords in the output before posting it anywhere.
