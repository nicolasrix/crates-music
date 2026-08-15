-- Per-track lyrics cache, resolved server-side.
--
-- Why the gateway holds this rather than each client fetching:
--   * one household, one cache — the second device and the CLI get every
--     lookup for free;
--   * the PWA in airplane mode cannot reach an external provider, so the
--     lyrics have to already be gateway-side to ride along with a
--     downloaded track;
--   * LRC text is parsed into structured lines exactly once, instead of
--     the web client and the TUI each reimplementing the format;
--   * one egress point: one User-Agent, one concurrency bound, one config
--     switch to stop talking to a third party entirely.
--
-- Why this pool (the recommender DB, not the OAuth one): the external
-- lookup key is artist + title + album + duration, and all four already
-- sit in `track_metadata` in this same file, populated for free by the
-- ingest pipeline. Co-locating means the resolver reads its lookup key
-- with a local join instead of a Navidrome round-trip.
--
-- Not partitioned by user, deliberately. Lyrics are a property of the
-- *catalog*, which the ownership boundary places on the shared side
-- alongside albums and tracks — unlike ratings, affinity or playlists.
-- A per-user copy would multiply identical rows and identical egress.
--
-- Not keyed by model_version (unlike track_embeddings): the mapping
-- track_id -> lyrics has nothing to do with which embedder is loaded.
-- Re-tagging a file in Navidrome mints a new track_id, so a stale row
-- orphans rather than going quietly wrong.
CREATE TABLE track_lyrics (
    track_id      TEXT    NOT NULL PRIMARY KEY,
    -- 'navidrome' (the file's own tags/sidecar) | 'lrclib' (external) |
    -- 'none' (looked, found nothing — see the negative-cache note below).
    source        TEXT    NOT NULL,
    -- How an external hit was matched: 'exact' (artist+title+album+
    -- duration), 'no_album' (album dropped — tag albums drift: "Deluxe",
    -- "Remastered"), 'search' (fuzzy, duration-guarded). Kept as an audit
    -- trail: when a wrong song's lyrics show up, this column says which
    -- tier to distrust. NULL for 'navidrome' and 'none'.
    match_kind    TEXT,
    -- 1 when lines_json carries timestamps. The whole point of the
    -- feature (line highlighting, click-to-seek) needs this to be 1;
    -- unsynced text is a consolation prize, so it is worth being able to
    -- count how often we land on it.
    synced        INTEGER NOT NULL,
    -- Provider says the track has no words at all. Distinct from 'none':
    -- an instrumental is a *successful* lookup with a definitive answer,
    -- so the client shows "Instrumental" instead of a retry affordance.
    instrumental  INTEGER NOT NULL,
    -- Timestamp-free text. Always populated when any words were found,
    -- including for synced hits, so a client that only wants a static
    -- view never has to strip timestamps itself.
    plain_text    TEXT,
    -- Normalized [{"start_ms": <i64>, "text": <string>}, ...], ordered by
    -- start_ms. NULL unless synced = 1. JSON rather than a child table:
    -- lines are only ever read as a whole document, never queried or
    -- joined individually, so a second table would buy nothing and cost
    -- a join on the hot path.
    lines_json    TEXT,
    -- Provider-side id (LRCLIB's numeric id), for re-fetch and for
    -- reporting a bad entry upstream.
    provider_id   TEXT,
    fetched_at    INTEGER NOT NULL,
    -- Re-resolution deadline. A source='none' row IS the negative cache:
    -- without it every play of an un-lyriced track re-hits the provider.
    -- Misses expire fast (the community DB grows), hits slowly.
    --
    -- Note the row is kept, not deleted, past this point: an expired hit
    -- is still the best thing to serve if the provider is unreachable
    -- (stale-if-error), and only the resolver decides whether staleness
    -- matters.
    expires_at    INTEGER NOT NULL
);

-- Supports the "what's worth re-resolving" sweep (expired misses first)
-- without a full scan.
CREATE INDEX track_lyrics_expires_idx ON track_lyrics (expires_at);
