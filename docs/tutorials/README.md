# Tutorials — start here

These guides assume **no prior experience** with servers, Docker, or the
command line beyond being willing to copy and paste. If a step needs
background, it links to a short explainer rather than assuming you already
know.

If you're a developer who wants to work *on* the code, you want
[GETTING-STARTED.md](../GETTING-STARTED.md) instead — it builds everything
from source. These tutorials use pre-packaged containers, which is far less
work.

## What this actually is

crates-music is **not** a music service. It has no music of its own. It sits
in front of [Navidrome](https://www.navidrome.org/), which is the program
that reads your music files and knows what you own. crates-music adds the
things Navidrome doesn't do: a nicer web and phone player, offline
downloads, recommendations, and separate accounts for everyone in the house.

So you need two things running: **Navidrome** (your library) and
**crates-music** (the player). Guide 01 does the first, guide 02 the second.

Everything runs on your own hardware, on your own network. Nothing is sent
to anyone else's servers.

## The guides

Do 01 and 02 in order. After that, pick whatever you care about.

| # | Guide | Time | Do I need it? |
|---|---|---|---|
| 01 | [Get your music library online](./01-navidrome.md) | ~20 min | **Yes** — unless Navidrome is already running |
| 02 | [Install crates-music](./02-install.md) | ~30 min | **Yes** — this is the main install |
| 03 | [Put it on your phone](./03-phone.md) | ~20 min | Optional, but it's the best part |
| 04 | [Turn on real recommendations](./04-recommendations.md) | ~1 hr + | Optional. Wants a decent computer |
| 05 | [Add family and guests](./05-household.md) | ~10 min | Optional. Only if others will use it |
| — | [When something goes wrong](./troubleshooting.md) | — | Keep this open in a tab |

Most of the "time" above is your computer working while you do something
else. Actual typing is a few minutes per guide.

## Explainers

Short, plain-language background. Read them when you're curious or when a
tutorial sends you here — you don't need them up front.

| Explainer | Answers |
|---|---|
| [How it all fits together](../explainers/how-it-works.md) | What each piece does and why there are several |
| [Why the security warnings?](../explainers/certificates.md) | Certificates, HTTPS, and why your browser complains |
| [Glossary](../explainers/glossary.md) | Every bit of jargon, defined plainly |

## Before you start

**You need a computer that stays on.** Whatever runs this has to be awake
when you want music. A spare laptop, a mini PC, a NAS, or a Raspberry Pi 4
or newer all work. Your daily laptop works too, but the music stops when
you close the lid.

**Everything happens on your home network.** By default nothing is reachable
from the internet, which is the safe default and the one these guides use.

**You'll use a terminal.** That's the black window where you type commands.
You don't need to understand the commands — every one is written out to
copy. If that's genuinely new to you, the
[glossary](../explainers/glossary.md#terminal) has a one-paragraph
orientation.

## A note on honesty

This project is a personal, self-hosted music player, not a polished
consumer product. These guides tell you where the sharp edges are rather
than pretending they don't exist. Where a step is fiddly, it says so — and
says whether you can skip it.
