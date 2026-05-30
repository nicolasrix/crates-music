-- All-but-the-Top (ABTT) whitening transform for the content ANN, one
-- row per model_version. The transform is *derived* data, refittable
-- from the raw `track_embeddings` corpus at any time (like the ANN
-- itself); this table just caches the fitted (mean, components) so a
-- gateway restart doesn't have to refit, and so an explicit refit stays
-- auditable via (n_samples, fitted_at_ms).
--
-- Tied to model_version: ABTT depends on the embedding-set composition,
-- and a model swap changes both the dimensionality and the cone, so the
-- transform never carries across versions.
--
-- `mean`       : dim * 4 little-endian f32 bytes (the corpus mean).
-- `components` : k * dim * 4 little-endian f32 bytes, row-major — the
--                top-k unit principal directions ABTT projects out.
-- Renormalization happens at apply time; nothing else is stored.

CREATE TABLE IF NOT EXISTS embedding_whitening (
    model_version   TEXT    NOT NULL PRIMARY KEY,
    dim             INTEGER NOT NULL CHECK (dim > 0),
    k               INTEGER NOT NULL CHECK (k >= 0),
    mean            BLOB    NOT NULL,
    components      BLOB    NOT NULL,
    n_samples       INTEGER NOT NULL CHECK (n_samples >= 0),
    fitted_at_ms    INTEGER NOT NULL
);
