-- Cross-modal text mean for the ABTT whitening transform.
--
-- The transform is fit on audio embeddings; CLaMP 3 text embeddings sit
-- at an offset (the modality gap), so a text station query centered by
-- the audio mean collapses against its neighbours. Storing the text-
-- modality mean lets the gateway center text queries by it instead, then
-- apply the same principal-direction removal — keeping text and audio
-- comparable in the whitened space.
--
-- Nullable: a row fitted before this column existed (audio-only) keeps
-- working; `text_mean IS NULL` makes text queries fall back to the audio
-- mean. Fitting it requires embedding a text-prompt corpus via the
-- sidecar, so it is populated lazily once the embedder is reachable.
--
-- `text_mean` : dim * 4 little-endian f32 bytes, same layout as `mean`.

ALTER TABLE embedding_whitening ADD COLUMN text_mean BLOB;
