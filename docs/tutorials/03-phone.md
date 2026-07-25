# 03 — Put it on your phone

**Goal:** crates-music on your phone's home screen, playing music, working
offline on the train.
**Time:** ~20 minutes.
**You need:** [guide 02](./02-install.md) finished and working on your
computer.

There's no app store download. The phone app **is** the web app, saved to
your home screen — same code, no separate install. Once saved it behaves
like a normal app: its own icon, no browser bars, lock-screen controls,
offline playback.

## The honest overview

This guide has two halves, and the second one is the fiddly part.

1. **Make the player reachable from your phone.** Straightforward.
2. **Make your phone trust the connection.** Annoying, and unavoidable if
   you want the home-screen app and offline playback.

The reason for part 2 is worth understanding before you fight it: phones
deliberately refuse to install web apps, or store music offline, over a
connection they can't verify. Your server currently uses a certificate it
issued to itself, which no phone trusts by default. That's not a flaw in
this project — it's the same rule that stops a café's Wi-Fi from installing
things on your phone.

[Why the security warnings?](../explainers/certificates.md) explains this
properly. You don't have to read it, but part 2 makes much more sense if
you do.

**You can stop after part 1** and use it in your phone's browser. You'll get
music, but no home-screen icon and no offline downloads.

---

## Part 1 — Reach it from your phone

Your phone needs to find your server. Pick whichever applies:

### Option A — Just use the address (quickest)

On your phone, connected to the **same Wi-Fi**, open:

```
https://192.168.1.42:8443
```

using the address from guide 01. Accept the certificate warning.

This is fine for testing and casual listening. It does **not** support the
home-screen install, because the certificate is issued for the name
`gateway.local`, not for a numeric address — so the name won't match even
if you later trust the certificate.

### Option B — Give it a name (needed for the home-screen app)

Your server needs a name your phone can look up. Best options, easiest
first:

**B1. Your router's DNS.** Most home routers can map a name to a device.
Look in the admin page for "Local DNS," "DNS Host Names," "Static DNS," or
"Host entries," depending on the brand. Add:

```
gateway.local  →  192.168.1.42
```

This is the cleanest answer — it works for every device on the network at
once, phones included.

**B2. Your computer's existing network name.** Macs and many Linux machines
already announce themselves as `<name>.local`. Find it:

```bash
hostname
```

If that prints `studio`, then `studio.local` probably already resolves from
your phone — try `https://studio.local:8443` and see. If it works, you'll
want the certificate reissued for that name (see the box below).

**B3. Not an option: editing the phone's hosts file.** iOS and Android
don't let you. If B1 and B2 both fail, skip to
[part 2, path C](#path-c--use-a-real-domain-name) or stay with option A.

> **Changing the name after first boot**
>
> The certificate is generated once, on first startup, for whatever
> `GATEWAY_HOSTNAME` was set to then. If you now want a different name,
> both the certificate and the login settings need refreshing — see
> [troubleshooting](./troubleshooting.md#i-need-to-change-the-hostname-after-first-boot).
> It's a five-minute fix, not a reinstall.

Confirm part 1 worked: your phone's browser loads the album list and plays
a song. Then continue.

---

## Part 2 — Make your phone trust it

Three paths. Read all three descriptions before choosing — the right one
depends on how much fiddling you'll tolerate.

| Path | Effort | Home-screen app | Offline | Warnings |
|---|---|---|---|---|
| **A** — browser only | none | ❌ | ❌ | every visit |
| **B** — trust the certificate | ~10 min per device | ✅ | ✅ | none after setup |
| **C** — real domain name | ~1 hour, once | ✅ | ✅ | none, on any device |

### Path A — Do nothing

Use it in the browser, accept the warning each time. Music works. Nothing
else in this guide applies.

### Path B — Trust the certificate on your phone

You copy your server's certificate to your phone and tell the phone to
trust it. Repeat per device.

**Step 1 — Get the certificate out of the container.**

On your server, in the `crates-music` folder:

```bash
docker compose cp gateway:/data/certs/gateway.local.pem ./gateway-cert.pem
```

That file is a **public** certificate, not a secret — it's the same one your
browser already receives on every connection. Emailing it to yourself is
fine. (The matching `-key.pem` file *is* secret. Leave it alone.)

**Step 2 — Get it onto the phone.** Email it to yourself, AirDrop it, or
put it in your cloud drive. Then:

**On iPhone/iPad:**
1. Open the file. iOS says a profile was downloaded.
2. **Settings → General → VPN & Device Management** → tap the profile →
   **Install**. Enter your passcode.
3. **This second step is mandatory and easy to miss:** **Settings → General
   → About → Certificate Trust Settings**, and switch on full trust for
   your certificate. Without it, iOS installs the certificate but keeps
   distrusting it, and nothing changes.

**On Android:**
1. **Settings → Security & privacy → More security settings → Encryption &
   credentials → Install a certificate → CA certificate**.
2. Android shows a stern full-page warning. Tap **Install anyway** and pick
   the file.
3. Android will now display a persistent "network may be monitored" notice.
   That's expected — you added a certificate authority, and Android tells
   the truth about what that means.

Exact menu paths move around between Android versions and manufacturers;
search your settings for "certificate" if the path above doesn't match.

**Understand what you just did:** you told your phone to trust anything
signed by that certificate. Since the private key lives only on your server
and never leaves it, the practical risk is low. But it is a real trust
decision, and you should only do it for a machine you control. To undo it,
delete the certificate from the same settings screen.

**Step 3 — Confirm.** Reload `https://gateway.local:8443` on the phone. No
warning, and a padlock in the address bar. If the warning persists, it's
almost always the iOS full-trust toggle in step 3, or a mismatch between
the name you're typing and the name on the certificate.

### Path C — Use a real domain name

If you own a domain, you can get a certificate every device already trusts,
with no per-device setup and no warnings anywhere. Your server stays on
your home network and unreachable from the internet — this uses a DNS-based
verification method that doesn't require opening any ports to the world.

This is more setup than this guide covers. The reverse-proxy configuration
under `docker/caddy/` and the
[TLS section of DEPLOYMENT.md](../DEPLOYMENT.md#tls) are the starting
points. If you have several devices to support, this is the option that
scales.

---

## Part 3 — Install to the home screen

With a trusted connection (path B or C), open the player on your phone.

**Android (Chrome, Edge, Brave, Samsung Internet):** open **Settings →
about** in the player, where an install offer appears with an **install**
button. If it's not there, use the browser menu → **Install app** / **Add
to Home screen**. Firefox and DuckDuckGo on Android can't install web
apps — use a Chromium-based browser.

**iPhone/iPad (Safari only):** tap the **Share** button, scroll to **Add to
Home Screen**, then **Add**. Chrome and Firefox on iOS can't do this;
Safari is the only option.

You now have an icon. Open it — no address bar, no browser tabs.

> If the **install** offer never appears on Android, the connection isn't
> fully trusted yet. That's the browser enforcing the rule from part 2, and
> it's the most common reason this step fails.

## Part 4 — Music offline

Downloads live on the phone, so they work in airplane mode.

1. Find an album or track you want on the plane.
2. Use its menu and choose **save for offline**.
3. Check progress on the **downloads** page in the sidebar.

Two things worth knowing:

- **Saved-for-offline tracks are protected.** They get their own storage
  budget and are never deleted automatically to make room. Ordinary
  listening also caches tracks, but those get cleared out as space runs
  low — only explicit saves are permanent.
- **Adjust the storage limit** in Settings if you plan to save a lot.

Test it honestly: turn on airplane mode, open the app from your home
screen, and play a saved track. If the app opens to a blank screen offline,
the home-screen install didn't complete — revisit part 3.

## What you should have

- ✅ The player on your phone's home screen, no browser bars
- ✅ Lock-screen and headphone controls while the screen is off
- ✅ Saved albums playing with no network at all

Music keeps playing when you lock the screen. Android handles this well;
iOS is more restrictive about background audio for home-screen web apps and
is occasionally less reliable about it than a native app would be.

## Next

- [04 — Turn on real recommendations](./04-recommendations.md)
- [05 — Add family and guests](./05-household.md)
- [Something's broken](./troubleshooting.md)
