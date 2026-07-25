# Glossary

Jargon you'll hit in these guides, in plain terms. Alphabetical.

### Bearer token

A long random string that acts as a password for programs rather than
people. Anything holding it gets access, which is why it's kept out of
logs. Yours lives inside the container and is only needed for occasional
maintenance commands.

### Certificate

A file your server presents to prove its identity over HTTPS. Contains a
public key and a claimed name. See
[why the security warnings?](./certificates.md) for the whole story.

### Certificate authority (CA)

An organisation browsers trust to verify identities before issuing
certificates. Your browser ships with a few hundred built in. Your home
server can't get a certificate from one, which is why you see warnings.

### Container

A packaged application bundled with everything it needs to run. Avoids
"works on my machine" — a container behaves identically anywhere Docker
runs. Both Navidrome and crates-music ship as containers.

### Docker / Docker Compose

**Docker** runs containers. **Docker Compose** starts several related
containers together from a single `docker-compose.yml` file describing how
they connect. Every `docker compose` command in these guides must be run
from the folder holding that file.

### Embedder / embedding

The **embedder** is the component that analyses audio. An **embedding** is
its output: a long list of numbers summarising how a track sounds. Similar
sounds produce similar numbers, which is what makes "find me more like
this" possible. See
[how it all fits together](./how-it-works.md#how-recommendations-work).

### Environment variable

A setting passed to a program by name, rather than written in a config
file. The `.env` file is a list of them, one per line, as `NAME=value`.

### Gateway

This project's server component — the thing between Navidrome and your
players. It handles accounts, recommendations, sync, and caching.

### Guest

A temporary account with no password, joined via a shared code. Guests can
play music and control the room they joined, but can't create playlists,
rate tracks, or influence recommendations. Access expires by itself.

### HTTPS

Encrypted web traffic. Does two jobs: hides the contents from anyone in
between, and proves who the server is. The second is the part that produces
warnings for home servers.

### IP address

A number identifying a device on a network, like `192.168.1.42`. Addresses
starting `192.168.`, `10.`, or `172.16`–`172.31` are private — reachable
only within your home network. Yours may change when the router restarts
unless you reserve it.

### `localhost`

Means "this same machine." `http://localhost:8443` from your server reaches
the player; the same address on your phone reaches your phone, and fails.
Browsers treat `localhost` as trusted without a certificate, which is why
some steps work there without warnings.

### mDNS / `.local` names

A way for devices to announce themselves on a local network without a DNS
server. Names ending in `.local` — `myserver.local` — usually come from
this. Works well on Apple devices, unevenly elsewhere.

### Navidrome

The separate program that reads your music files, builds a catalogue, and
serves audio. crates-music sits in front of it and has no library of its
own. <https://www.navidrome.org/>

### Pinning / saving for offline

Marking a track to be kept on a device permanently. Pinned tracks get their
own storage budget and are never automatically deleted, unlike ordinary
cached tracks which are cleared out as space runs low.

### PWA (Progressive Web App)

A website that can be installed to your home screen and behave like a
normal app — own icon, no browser bars, works offline. The phone version of
this player is a PWA, which is why there's no app store download.

### Reverse proxy

A server that sits in front of another and forwards requests, typically to
handle certificates or expose several services on one address. Optional
here; relevant if you use a real domain name.

### Room

A shared playback session. Guests join a host's room and control the same
queue — the party-jukebox model, as opposed to each person having a private
session.

### Scrobble

A record that you played a track. Feeds listening history and
recommendations. The name comes from Last.fm.

### Secure context

A browser rule: certain powerful features only work over HTTPS (or on
`localhost`). Includes app installation, offline storage, and lock-screen
controls — which is why plain HTTP isn't an option here.

### Service worker

A small script a website installs in your browser so it can work offline.
Part of what makes the phone app function without a network. Requires a
trusted HTTPS connection.

### Subsonic API

The standard language crates-music uses to talk to Navidrome. Originally
from an older music server; now widely supported, which is why many music
apps can talk to Navidrome.

### Terminal

The text window where you type commands — **Terminal** on Mac and Linux,
**PowerShell** or **Windows Terminal** on Windows. Commands run one per
line; press Enter to run. You can paste with `Ctrl+Shift+V` (Linux),
`Cmd+V` (Mac), or `Ctrl+V` (Windows). Nothing in these guides requires you
to understand the commands — copying them is enough.

### Token

Generic term for a secret string granting access. The **setup token**
appears once at first startup and creates your password. A **guest code**
is a token you hand to visitors. An **access token** is what your browser
holds after signing in.

### Volume

Docker's word for storage that outlives a container. Your accounts,
settings, and downloads live in one called `crates-music_gw-data`, which
survives rebuilds and updates — and is deleted by
`docker compose down -v`.

## Related

- [Why the security warnings?](./certificates.md)
- [How it all fits together](./how-it-works.md)
- [Tutorials](../tutorials/README.md)
