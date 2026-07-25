# Why the security warnings?

You set up a music player on your own computer, on your own network, and
your browser reacted like you'd wandered onto a phishing site. That's
confusing, and the confusion is reasonable. Here's what's actually going
on.

**Short version:** the connection genuinely is encrypted. What your browser
is complaining about is that it can't independently verify *who* it's
talking to — and for a server on your own network, there is no way it
could. The warning is your browser being honest about the limits of what it
knows, not a sign that something is wrong.

## HTTPS does two separate jobs

The padlock in your address bar is doing two unrelated things, and people
usually only think about the first:

1. **Encryption** — nobody between you and the server can read the traffic.
2. **Identity** — the server is who it claims to be.

Job 1 is easy. Any two computers can agree on encryption keys with no
outside help, and it works perfectly for your music player.

Job 2 is the hard one, and it's the one causing the warning.

## The identity problem

Say your browser connects to something calling itself `gateway.local`. It
gets back a certificate that says "I am gateway.local."

But anyone can *say* that. Someone on your Wi-Fi could intercept the
connection and present their own certificate claiming the same thing. The
browser needs a reason to believe this one.

On the public internet, that reason is a **certificate authority** — a
company like Let's Encrypt whose job is verifying that whoever's asking for
a certificate for `example.com` actually controls `example.com`. Your
browser ships with a built-in list of a few hundred such authorities it
trusts. If one of them signed the certificate, the browser accepts it.

## Why that doesn't work for you

No certificate authority will ever issue a certificate for `gateway.local`.

They can't. `gateway.local` isn't a real internet address — it's a private
name that means something different on every network in the world. On your
network it's your music server; on mine it's nothing. Nobody can verify
ownership of a name that isn't globally unique, so nobody will vouch for
it.

So your server does the only thing available: it signs its own certificate.
This is a **self-signed certificate**, and it amounts to a stranger handing
you an ID card they printed themselves. The encryption works fine. The
identity claim just has nothing behind it.

Your browser can't tell the difference between "self-signed certificate
belonging to the server I actually meant" and "self-signed certificate
belonging to an attacker." So it warns you, and it's right to.

**You** can tell the difference, because you set the thing up. That's why
clicking through is a reasonable choice on your own network, and a terrible
one on a café's Wi-Fi.

## So why not just use plain HTTP?

If the identity guarantee is hollow anyway, why bother with HTTPS at all?
Two reasons.

**Your password would travel in the clear.** Every device on your network —
and anyone who's joined your Wi-Fi — could read it. Home networks aren't as
private as they feel.

**Modern browser features simply won't work.** Over the last decade
browsers have moved their more powerful capabilities behind a rule called
"secure context," meaning HTTPS only. That includes nearly everything that
makes this a *player* rather than a webpage:

| Feature | Needs HTTPS |
|---|---|
| Installing to your home screen | ✅ |
| Offline playback | ✅ |
| Lock-screen and headphone controls | ✅ |
| Reserving storage that won't be evicted | ✅ |
| Signing in via the browser | ✅ |

Over plain HTTP you'd get a webpage that plays music while it's open, and
nothing else. HTTPS isn't optional here — it's what the phone experience is
built on.

## The `localhost` exception

Browsers make one exception: `http://localhost` is treated as secure
without any certificate. The reasoning is that traffic to localhost never
leaves the machine, so there's nothing to intercept.

This is why developers can work without certificates, and why the install
guide has you set up your password over localhost without fuss.

It does nothing for your phone. Your phone's `localhost` is your phone.

## Your three options

Ordered by effort:

### 1. Click through the warning

Fine for casual use on a computer. Encryption still applies. You'll see the
warning periodically, and the browser will refuse to install the app or
store music offline.

### 2. Trust your server's certificate on each device

Add your server's certificate to a device's list of trusted authorities.
That device then treats your server as legitimate — no warnings, and all
the features unlock.

The trade-off is real and worth stating: you're telling that device to
trust anything signed by that certificate. Since the matching private key
never leaves your server, the practical risk is low. But if someone stole
that key, they could impersonate other sites to that device. Only do this
for machines you control, and remove it when you stop using them.

Per-device setup, so it gets tedious past a handful of devices.
[Guide 03 walks through it](../tutorials/03-phone.md#path-b--trust-the-certificate-on-your-phone).

### 3. Use a real domain name

If you own a domain, you can get a genuine certificate from Let's Encrypt
for a name like `music.yourdomain.com`, and every device trusts it
automatically with zero setup.

The surprising part: **your server does not need to be reachable from the
internet.** There's a verification method called DNS-01 where you prove
domain ownership by adding a DNS record, rather than by receiving an
incoming connection. Your server stays firewalled off from the world, the
domain points at a private address that only resolves on your network, and
you still get a trusted certificate.

This is the best answer if you have several devices or non-technical people
using it — nobody ever sees a warning. It's also the most setup. See
[DEPLOYMENT.md](../DEPLOYMENT.md#tls).

## What about mkcert?

If you're reading the developer docs you'll see
[mkcert](https://github.com/FiloSottile/mkcert) mentioned. It's a tool that
automates option 2: it creates a small private certificate authority on
your machine, adds it to your system's trust store, and issues certificates
from it. Everything on that machine then trusts them.

It's convenient for development on one computer. It's less helpful across
several devices, since the authority still has to be installed on each one
by hand.

The Docker install doesn't use mkcert — the container generates a plain
self-signed certificate on first startup, so there's nothing extra to
install.

## Quick answers

**Is my music encrypted in transit?** Yes, in every option here. Encryption
was never the problem.

**Is clicking through dangerous?** On your own home network, connecting to
your own server: not meaningfully. The habit is the risk — get comfortable
dismissing certificate warnings and you'll dismiss one that mattered.

**Can I make the warning go away for good?** Options 2 and 3, yes.

**Why does my phone refuse to install the app even after I click through?**
Deliberate. Browsers don't accept a manually-dismissed warning as
equivalent to a trusted connection, precisely because users dismiss
warnings reflexively. You need option 2 or 3.

**I trusted the certificate but it still warns.** On iPhone, installing the
profile is only half of it — you must also enable full trust under
**Settings → General → About → Certificate Trust Settings**. Otherwise the
name on the certificate doesn't match the address you typed.

## Related

- [Guide 03 — Put it on your phone](../tutorials/03-phone.md)
- [Glossary](./glossary.md)
- [DEPLOYMENT.md § TLS](../DEPLOYMENT.md#tls) — production certificate setup
