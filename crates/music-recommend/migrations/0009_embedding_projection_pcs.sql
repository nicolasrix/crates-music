-- Per-point PCA components alongside the UMAP (x, y) coords.
--
-- Background: UMAP is excellent at making clusters legible but its
-- output dimensions carry no quantitative meaning — distances between
-- clusters are arbitrary, and the layout is rotation/reflection-
-- unstable across reruns. PCA on the same 512-D CLAP vectors is a
-- different read: linearly-orthogonal axes ordered by variance.
-- Storing the first four PCs alongside UMAP gives the diagnostics
-- page a continuous-valued channel (colour, size, opacity) that
-- reflects honest dimensions of the latent space — the variance UMAP
-- discarded when collapsing to 2D.
--
-- Nullable so a rerun of `embedder.reduce` against this migration
-- writes the values, but pre-migration rows keep working — they
-- simply offer no PC dimensions to the colour-by dropdown.
--
-- Tied to the same `proj_version` as (x, y): PCA, like UMAP, depends
-- on the embedding-set composition at the moment of computation.
-- Pairing them under one version string keeps the "everything in this
-- projection is internally consistent" invariant.

ALTER TABLE embedding_projection_2d ADD COLUMN pc1 REAL;
ALTER TABLE embedding_projection_2d ADD COLUMN pc2 REAL;
ALTER TABLE embedding_projection_2d ADD COLUMN pc3 REAL;
ALTER TABLE embedding_projection_2d ADD COLUMN pc4 REAL;
