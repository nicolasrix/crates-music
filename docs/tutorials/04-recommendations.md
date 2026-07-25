# 04 — Turn on real recommendations

**Goal:** "play things that sound like this" that actually works, and
stations you describe in words ("rainy sunday afternoon").
**Time:** ~30 minutes of setup, then hours of unattended processing.
**Entirely optional.** Everything else works without it.

## Why your recommendations are nonsense right now

The default install runs a **placeholder** recommendation engine. It's a
stand-in that produces valid-looking but meaningless results, so the rest of
the system can be tested without a heavyweight setup. Any "similar tracks"
you've seen so far were effectively random.

To get real ones, you swap in the actual engine. It listens to the first
couple of minutes of every track in your library and turns each into a
long list of numbers describing how it sounds. Tracks with similar numbers
sound similar. That's the whole idea.

Two things fall out of it for free:

- **Sounds-like search** — find the nearest-sounding tracks to whatever is
  playing, or to a whole album.
- **Text stations** — the same maths maps *descriptions* into the same
  space, so "melancholy piano" can be compared against the sound of your
  music directly.

Neither uses genre tags, so it works on badly-tagged libraries, and it
notices when two things sound alike despite being filed differently.

## Is it worth it for you?

| | |
|---|---|
| **Disk** | ~2 GB for the model, plus a little per track |
| **Memory** | 6 GB for the analysis engine while it runs |
| **Time** | Roughly 3–5 seconds per track on a normal CPU. A 10,000-track library is most of a day |
| **Hardware** | Any modern computer. A graphics card makes it ~6× faster but isn't required |
| **Ongoing cost** | None. New tracks trickle through; the heavy pass happens once |

On a Raspberry Pi this is not realistic. On a laptop or desktop left running
overnight, it's fine.

**Everything else keeps working while it runs.** Browsing, playing, and
downloads are unaffected — recommendations simply improve as tracks are
processed.

## Step 1 — Download the model

The analysis engine needs a trained model file, which is too large to ship
with the project.

Get the **CLaMP 3** weights from Hugging Face:
<https://huggingface.co/sanderwood/clamp3>

Download the file whose name contains **`saas`**. It's around 2 GB and its
name is long and ugly:

```
weights_clamp3_saas_h_size_768_t_model_FacebookAI_xlm-roberta-base_t_length_128_a_size_768_a_layers_12_a_length_128_s_size_768_s_layers_12_p_size_64_p_length_512.pth
```

> ⚠️ **Do not rename this file.** The filename is used as the version
> identifier for everything analysed with it. Renaming it to something
> sensible like `clamp3.pth` makes the system treat previously-analysed
> tracks as belonging to a different model, and it will silently
> re-analyse your entire library. This has bitten this project before.

Put it in a `models` folder somewhere on your server:

```bash
mkdir -p ~/crates-config/models
mv ~/Downloads/weights_clamp3_saas_*.pth ~/crates-config/models/
```

## Step 2 — Point the configuration at it

In your `.env` file, add:

```bash
CRATES_CONFIG_DIR=/home/yourname/crates-config
```

Use the **full path** to the folder that *contains* `models` — not the
`models` folder itself. `~` shortcuts don't work here; write it out.

## Step 3 — Clear the old index

The placeholder engine and the real one describe tracks with differently
sized lists of numbers, and the search index can't hold both. It must be
deleted so it rebuilds at the new size.

Stop the stack and remove it:

```bash
docker compose down
docker compose run --rm --entrypoint sh gateway -c \
  'rm -f /data/state/gateway-state.ann /data/state/gateway-state.ann.keys'
```

This deletes only the search index, which is rebuilt automatically. Your
accounts, downloads, and settings are untouched.

## Step 4 — Start with the real engine

```bash
docker compose -f docker-compose.yml -f docker-compose.clamp3.yml up --build -d
```

The extra `-f` adds an overlay that swaps the placeholder for the real
engine. **You must include both `-f` flags every time you start the stack
from now on** — otherwise you silently get the placeholder back.

Make that easier on yourself:

```bash
# Add to ~/.bashrc or ~/.zshrc
alias crates='docker compose -f docker-compose.yml -f docker-compose.clamp3.yml'
```

Then it's just `crates up -d`, `crates logs -f`, and so on.

This build downloads several more GB. Check it came up correctly:

```bash
docker compose logs gateway | grep -i "embedder"
```

You want a line reporting `dim=768`:

```
INFO embedder: probe ok model=weights_clamp3_saas_... dim=768 device=cpu
```

`device=cpu` is normal and fine. `device=cuda` means a graphics card was
found and it'll be much faster. If you see `dim=512`, the overlay didn't
take effect — check both `-f` flags are present.

## Step 5 — Queue your library for analysis

Nothing is analysed until asked. A bundled script queues everything.

First get your server's access token:

```bash
docker compose exec gateway cat /data/state/bearer.token
```

Then, from the `crates-music` folder:

```bash
NAVIDROME_URL=http://192.168.1.42:4533 \
NAVIDROME_USERNAME=your-navidrome-username \
NAVIDROME_PASSWORD=your-navidrome-password \
GATEWAY_URL=https://localhost:8443 \
GATEWAY_BEARER=the-token-from-above \
INSECURE=1 \
python3 scripts/enqueue_all_tracks.py
```

It walks every album and queues every track. Re-running is harmless —
already-processed tracks are skipped.

> `INSECURE=1` turns off certificate checking for this one command. It's
> acceptable here because you're connecting to your own machine over
> `localhost`. If you'd rather not, set `CA_CERT_PATH` to your certificate
> file instead — the script prefers that.

## Step 6 — Wait

Analysis runs quietly in the background at low priority. Watch it drain:

```bash
docker compose logs -f embedder
```

Or open **Settings → ingest** in the player (admin accounts only), where
the queue depth counts down in real time.

Expect several hours for a large library. It survives restarts — stop and
start the stack freely, and it picks up where it left off.

Recommendations improve gradually. Tracks that haven't been analysed yet
fall back to tag-based similarity rather than failing.

## Step 7 — Try it

Once a decent chunk is processed:

- Open any album and press **start station** — it plays tracks that sound
  like that album, drawn from across your whole library.
- Let a queue play to the end. Autoplay keeps going with similar-sounding
  music instead of stopping.
- Open **station** in the sidebar and type a description: `late night
  driving`, `aggressive drums`, `warm acoustic folk`.

**Calibrate your expectations on stations.** Concrete, musical descriptions
work well — genres, instruments, tempo, texture. Abstract or emotional
prompts ("sunny afternoon", "music for studying") are noticeably weaker,
because the underlying model represents that kind of language poorly. This
is a known limitation of the model rather than a configuration problem, so
lean concrete and you'll get better results.

## If you have an AMD graphics card

There's a GPU variant, roughly 6× faster (a batch that takes 31 seconds on
CPU takes 5 on a tested RDNA4 card):

```bash
docker compose -f docker-compose.yml -f docker-compose.clamp3-rocm.yml up --build -d
```

It needs ROCm-capable hardware and correct render/video group permissions —
see [DEPLOYMENT.md](../DEPLOYMENT.md#running-with-clamp-3-768-dim-production).
Confirm success by checking for `device=cuda` in the probe line from step 4.
If it says `device=cpu`, it silently fell back and you're getting no
benefit.

The analysis engine can also run on a *different* machine from the player —
a gaming PC does the work while a small always-on box serves the music. See
[split-host deployment](../DEPLOYMENT.md#split-host-deployment-gateway--remote-embedder).

## Next

- [05 — Add family and guests](./05-household.md)
- [Something's broken](./troubleshooting.md)
