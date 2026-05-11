-- Track metadata cache, sibling to track_embeddings.
--
-- Why a cache at all: the gateway already pays for one Subsonic getSong
-- per ingested track (as a free side-effect of fetching audio); folding
-- the response into a local table means downstream features (diversity
-- rerank, multi-edition dedup, song stats, behaviour-based recs) can
-- read artist/album/title locally instead of hammering Navidrome on
-- every recommend response.
--
-- Why not model_version-keyed (unlike track_embeddings): the mapping
-- track_id → metadata doesn't depend on which CLAP checkpoint is loaded.
-- Re-tagging a file in Navidrome regenerates track_id, so old rows
-- orphan rather than going stale; the orphan is never queried because
-- the embedding for that id is also gone.
--
-- title_normalized stored on disk so dedup queries don't pay a
-- normalization cost per row at query time.
CREATE TABLE track_metadata (
    track_id          TEXT    NOT NULL PRIMARY KEY,
    artist_id         TEXT,
    artist            TEXT    NOT NULL,
    album_id          TEXT,
    album             TEXT,
    title             TEXT    NOT NULL,
    title_normalized  TEXT    NOT NULL,
    duration_seconds  INTEGER,
    genre             TEXT,
    year              INTEGER,
    track_number      INTEGER,
    disc_number       INTEGER,
    bpm               INTEGER,
    musical_key       TEXT,
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL
);

CREATE INDEX track_metadata_artist_id_idx ON track_metadata (artist_id);
CREATE INDEX track_metadata_album_id_idx  ON track_metadata (album_id);
