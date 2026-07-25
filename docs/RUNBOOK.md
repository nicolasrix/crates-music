# Runbook

Operational quick-reference: what to run when deploying, checking
health, recovering, or diagnosing the live stack. Procedures here are
checklists — the full explanations live in
[DEPLOYMENT.md](./DEPLOYMENT.md) (operator guide) and
[CONFIGURATION.md](./CONFIGURATION.md) (every knob).

## Topology

Single-host is the simple case: gateway, reverse proxy and embedder all
in one compose stack. The recommender is the only component that wants a
GPU, so it can also be split onto a second host — useful when the box
holding the library is CPU-only:

| Role | Runs | Notes |
|---|---|---|
| Gateway host | `crates-gateway` + `crates-caddy` | May be CPU-only. Its `EMBEDDER_URL` points at whichever host runs the sidecar — the same stack, or another box over the LAN. NAS app platforms usually want their own Custom-App-style deployment path rather than raw `docker compose` over SSH, which may not survive platform upgrades. |
| Embedder host (optional) | embedder sidecar (`crates-embedder`, CLaMP 3 ROCm) on `:9000` | Only needed to put inference on a GPU. See [DEPLOYMENT.md → Bringing up the GPU box](./DEPLOYMENT.md#bringing-up-the-gpu-box-embedder). |

## Health checks

| Check | Command | Healthy looks like |
|---|---|---|
| Gateway liveness | `curl -k https://gateway.local:8443/healthz` | `200` always (process up) |
| Gateway readiness | `curl -k https://gateway.local:8443/readyz` | `200`; `503` = degraded — body says whether `navidrome` or `embedder` is the failing check |
| Embedder | `curl http://<gpu-box>:9000/healthz` | `dim: 768`, `device: "cuda"` — **`device: "cpu"` means silent GPU fallback**, investigate ROCm |
| Container status | `docker ps` on the host | gateway container `healthy` (its `HEALTHCHECK` is `/readyz`) |

Details and the `/readyz` response shape:
[DEPLOYMENT.md → Health endpoints](./DEPLOYMENT.md#health-endpoints).
Runtime diagnostics (traces, histograms, ingest queue depth, browser
RUM) live at the web app's `/diagnostics` page and
`GET /v1/diagnostics/*` ([API.md](./API.md)).

## Deploy / update

**Single-host (compose):** backup first, then

```bash
git pull && docker compose build && docker compose up -d
```

See [DEPLOYMENT.md → Updates](./DEPLOYMENT.md#updates).

**Gateway image → a remote host (no registry):**

```bash
# 1. Build locally from the release branch
docker compose build gateway

# 2. Stream it to the host (the NAS admin user needs sudo for docker)
REMOTE_DOCKER="sudo docker" ./scripts/ship-image.sh crates-music/gateway:dev nas-host

# 3. Apps → Custom App → Save/restart the app on the host
```

**Verify after any deploy:** `/readyz` is 200, boot log shows the
embedder probe (`embedder: probe ok … dim=768`), web app loads, a
track plays.

## Embedding model / dim bump (e.g. CLAP→CLaMP 3)

A dim change is **non-migratable** for the ANN sidecar. In order:

1. Rebuild the gateway image from the branch and ship it (above) —
   `RECOMMEND_EMBEDDING_DIM` is read by `gen_config.py` **baked into the
   image**; an env-only change against an old image is a silent no-op.
2. Set `RECOMMEND_EMBEDDING_DIM` (Custom App YAML / `.env`, per platform).
3. Stop the gateway; wipe the ANN sidecar: `gateway-state.ann` +
   `gateway-state.ann.keys`. Do **not** wipe the `embedding_whitening`
   table — a stale-dim cached row is detected at boot and refit
   automatically.
4. Start; re-embed via `scripts/enqueue_all_tracks.py`. The recommender
   runs degraded (tag-only) until the queue drains — watch
   `GET /v1/diagnostics/queue_depth`.
5. Keep the **upstream checkpoint filename** — the filename IS the
   embedding `model_version`; renaming split-brains the store.

Full context: [DEPLOYMENT.md → Running with CLaMP 3](./DEPLOYMENT.md#running-with-clamp-3-768-dim-production).

## Backup / restore / rollback

| Action | Command |
|---|---|
| Backup | `./scripts/backup.sh <state-dir> <output-dir>` — captures `gateway-state.sqlite` (OAuth, sync, play counts), `gateway-state.recommend.sqlite` (embeddings), `certs/`. Caches are deliberately excluded (derivable). |
| Restore | Stop the gateway first, then `./scripts/restore.sh [--force] <archive.tar.gz> <dest-dir>` — sha256-verified against the manifest. Restoring over open DBs corrupts both. |
| Drill | `./scripts/tests/backup-roundtrip.sh` — run it after changing anything in the backup path. |
| Rollback (bad image) | Re-ship the previous image tag and restart; state lives in the named volume, not the image. If state was damaged, restore the pre-deploy backup. |

Cron snippet (daily, keep 14) and the full walkthrough:
[DEPLOYMENT.md → Backups](./DEPLOYMENT.md#backups).

## Account recovery (no email)

There is no email or recovery-code path — by design. Two cases:

- **A household member forgot their password.** An admin resets it from
  the web Account → Users panel, or directly:
  `POST /v1/admin/users/<id>/password` with `{"password":"<≥12 chars>"}`.
  The hash is rewritten in place; the account keeps its id and data.

- **The owner forgot the *master* password.** Run the host-only
  subcommand on the gateway host — host access is the root of trust:

  ```bash
  # bare metal
  echo -n 'new-master-password' | \
    music-gateway --config /path/to/gateway.toml reset-master-password

  # NAS / docker (the gateway image is the same binary)
  echo -n 'new-master-password' | \
    sudo docker exec -i crates-gateway \
      music-gateway --config /config/gateway.toml reset-master-password
  ```

  Pipe the password (as above) so it stays out of shell history; omit the
  pipe and it's read interactively from stdin. The command validates the
  length (≥ 12), rewrites the Argon2 hash on `users.id=1` **in place**
  (never delete/recreate — that would cascade-drop the owner's data), and
  exits without starting the server. It refuses to run on a gateway that
  was never bootstrapped (complete `/oauth/setup` first). Existing
  sessions keep working; only the password used at `/oauth/login`
  changes.

## Common issues

| Symptom | Cause | Fix |
|---|---|---|
| `/v1/recommend/next` 404 for every seed | Gateway booted with the embedder unreachable → degraded mode. **No auto-retry.** | Start the embedder, then restart the gateway. |
| Recommendations suddenly slow (~6× ingest) | Embedder fell back to CPU (`/healthz` → `device: "cpu"`). | Check ROCm on the GPU box; `HIP_VISIBLE_DEVICES` pinning; restart the sidecar. |
| First embed after sidecar (re)start takes ~5 s | MIOpen JIT kernel compile, then cached (~230 ms warm). | Expected — not a regression. See [DEPLOYMENT.md → Debugging](./DEPLOYMENT.md#debugging). |
| Gateway still on 512-dim after setting `RECOMMEND_EMBEDDING_DIM=768` | Old gateway image — its baked `gen_config.py` ignores the var. | Rebuild + ship the image (see model-bump runbook above). |
| Changed `redirect_uris` on a `[[oauth.clients]]` block has no effect | Boot registration is insert-only; updates to an existing `client_id` are silently dropped. | `DELETE FROM oauth_clients WHERE client_id = '…';` then restart the gateway. |
| Text stations return near-identical results for different prompts | Embedder image missing `sentencepiece`/tokenizer prebake (every word → `<unk>`). | Rebuild the embedder image; verify with two different prompts → distinct results. Audio embeddings are unaffected. |
| Browser cert warnings / web app won't install as PWA | mkcert cert expired (annual regeneration, not automatic) or CA not trusted on the device. | Regenerate via `./scripts/dev-certs.sh`; `mkcert -install` on each device. See [DEPLOYMENT.md → TLS](./DEPLOYMENT.md#tls). |
| CLI gets 401 from the gateway | OAuth tokens revoked/expired beyond refresh. | `music auth status`, then `music auth login` (RFC 8628 device flow — approve in a logged-in browser). |

## Escalation

Single-operator homelab — there is no on-call. When something is truly
wedged: backup the state volume, capture `docker logs` from the
gateway + embedder, and check the trace store
(`GET /v1/diagnostics/traces`) before restarting things that hold
state.
