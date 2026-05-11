-- Per-track recency clock for the recommender's MMR pipeline.
--
-- Why a dedicated table (vs. deriving from `events`): MMR's recency
-- penalty needs O(1) per-candidate lookup of "when was this last
-- played." Scanning the event log for MAX(occurred_at) per candidate
-- would dominate the recommend hot path. This is the precomputed view.
--
-- Why not store album-level counts here: A2's deliberate decision is
-- "derive on read." Track-level data sums up cheaply when needed.
--
-- Why MAX semantics in the UPSERT (handled at write site): scrobbles
-- can arrive out of order — offline batches, retries. We never want
-- the recency clock to move backwards. See
-- PlayHistoryStore::record_submission for the merge clause.
--
-- Why WITHOUT ROWID: track_id is the natural primary key and we only
-- ever do point lookups + upserts. WITHOUT ROWID skips the redundant
-- rowid index and is the SQLite-recommended layout for this access
-- pattern.
CREATE TABLE play_history (
    track_id        TEXT    PRIMARY KEY,
    last_played_ms  INTEGER NOT NULL
) WITHOUT ROWID;
