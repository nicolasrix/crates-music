-- Third UMAP axis, for the latent-space scatter's "colour by → UMAP z"
-- mode. Nullable so any existing 2D projection continues to work
-- without rewriting; only a fresh 3-component reducer run populates
-- this column.
--
-- The table is still named `embedding_projection_2d` — the canvas axes
-- are still (x, y); `z` is a colour-only channel, not a third geometry
-- axis. Renaming the table would force a migration across every
-- consumer for a layout that's still fundamentally 2D on the screen.
--
-- Same `proj_version` tying that's been in place since 0006 holds: a
-- 3D run lives under `umap-v1-…-d3` and a 2D run under the un-suffixed
-- form, so the two coexist for diffability.

ALTER TABLE embedding_projection_2d ADD COLUMN z REAL;
