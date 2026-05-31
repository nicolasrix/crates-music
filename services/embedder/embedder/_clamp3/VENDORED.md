# Vendored: CLaMP 3 inference code

This directory contains a vendored snapshot of the audio-side inference code
from CLaMP 3, with patches required to integrate with the `crates-music`
embedder sidecar. Everything else (training scripts, CLI wrappers, the
symbolic-music branch, the image branch) is intentionally omitted — see
the per-file map below for what is included and what was dropped.

## Source

- Upstream repository: <https://github.com/sanderwood/clamp3>
- Upstream commit: `9016d2b0c8d12d1aa79c2e0ab201e6822bdc83a8` (2026-05-30)
- Upstream HEAD at the time: "Update clamp3_eval.py"

## Citation

If you publish work that uses these embeddings, please cite the paper:

> Wu, S., Wang, Z., Chen, R., Guo, X., Li, T., Wang, Y., Liu, X., Liu, Y.,
> & Cheng, K. (2025). *CLaMP 3: Universal Music Information Retrieval
> Across Unaligned Modalities and Unseen Languages.* Findings of the
> Association for Computational Linguistics: ACL 2025.
> <https://aclanthology.org/2025.findings-acl.133/>

## Licensing

This vendored tree carries **two licenses**:

| Scope | License | File |
|---|---|---|
| Default (5 of 6 vendored `.py` files) | MIT | `LICENSES/MIT-LICENSE` |
| `musichubert_config.py` (HuggingFace-derived) | Apache 2.0 | `LICENSES/APACHE-2.0-LICENSE` |

A third, more restrictive license applies at runtime but is **not** redistributed
here because we do not bundle the weights:

- **MERT-v1-95M weights** (downloaded from <https://huggingface.co/m-a-p/MERT-v1-95M>
  on first run) are released under **CC-BY-NC-4.0**. This means the audio
  embedding path is fine for personal / research use but is **not licensed
  for commercial use** without separate arrangements with the MERT authors.
- The CLaMP 3 weights themselves (`weights_clamp3_saas_*.pth` from
  <https://huggingface.co/sander-wood/clamp3>) are MIT — the restrictive
  license comes from the MERT dependency, not from CLaMP 3 itself.

## File-by-file map

| Local file | Upstream path | Lines (local / upstream) | License | Modifications |
|---|---|---|---|---|
| `config.py` | `code/config.py` | 30 / 79 | MIT | Slimmed to inference-relevant fields only (audio + clamp3 + M3 patch geometry referenced by M3PatchEncoder). Dropped training paths, learning rates, wandb keys, weight-path templates. |
| `model.py` | `code/utils.py` | 137 / 573 | MIT | Slimmed to `M3PatchEncoder` + `CLaMP3Model` only. Dropped `ClipLoss`, `M3Patchilizer`, `M3TokenDecoder`, `M3Model`, training helpers (`split_data`, `mask_patches`, `remove_instrument_info`), and the horovod / `torch.distributed` imports they pulled in. Trimmed `CLaMP3Model`: removed `loss_fn`, `set_trainable`, `forward`, and the side-loaded-M3-checkpoint code path. Default `load_m3=False`. |
| `musichubert_config.py` | `preprocessing/audio/configuration_musichubert.py` | 273 / 273 | Apache 2.0 | **Bit-identical to upstream.** Preserves the original copyright header (© 2021 The Fairseq Authors and The HuggingFace Inc. team). |
| `musichubert_model.py` | `preprocessing/audio/MusicHubert.py` | 440 / 429 | MIT | One-line import fix: `from configuration_musichubert import MusicHubertConfig` → `from .musichubert_config import MusicHubertConfig` (relative import for vendored package layout). Plus our 11-line provenance header. No model code changes. |
| `feature_extractor.py` | `preprocessing/audio/hf_pretrains.py` | 175 / 232 | MIT | Dropped `Data2vecFeature` and `SpeechHuBERTFeature` classes (alternative feature extractors we never instantiate). Dropped imports they used (`Data2VecAudioConfig`, `Data2VecAudioModel`, `HubertModel`, `AutoModel`). Relative imports for `MusicHubertConfig` / `MusicHubertModel`. |
| `audio_io.py` | `preprocessing/audio/MERT_utils.py` | 142 / 180 | MIT | Renamed `load_audio` → `load_audio_bytes` and changed the first argument from `file_path: str` to `raw_bytes: bytes`. Replaced `torchaudio.load(file_path)` with `soundfile.read(io.BytesIO(raw_bytes), dtype='float32', always_2d=True)` — torchaudio 2.9 dropped its built-in backends in favor of `torchcodec`, which has ABI issues with ROCm wheels + FFmpeg 8 on this deployment target. Dropped `chunk_audio` (unused), `find_audios` (CLI helper we replace), and the `mido` / `argparse` imports they pulled in. |

## Files we deliberately did NOT vendor

For the audit trail — these upstream files are part of the CLaMP 3 repo
but are not used by the inference sidecar and were not copied:

- `clamp3_embd.py`, `clamp3_eval.py`, `clamp3_score.py`, `clamp3_search.py` —
  CLI wrappers. We call the model classes directly from `clamp3_backend.py`.
- `utils.py` (top-level) — modality detection, `extract_audio_features`
  subprocess chain. Replaced by in-process calls.
- `code/extract_mert.py`, `code/extract_clamp3.py`,
  `preprocessing/audio/extract_mert.py` — directory-in/directory-out batch
  scripts. Replaced by `Clamp3Embedder.embed_audio(bytes)`.
- `code/train_*.py`, `classification/`, `inference/` — training / downstream
  classification / evaluation. Out of scope for the sidecar.
- `preprocessing/abc/`, `preprocessing/midi/` — symbolic music preprocessing.

## Re-syncing with a newer upstream

1. Clone or `git fetch` the upstream repo at the target commit.
2. For each file in the table above, run a textual diff against the new
   upstream version. Files marked "bit-identical to upstream" must remain
   bit-identical or the Apache 2.0 modification-notice obligation kicks in.
3. Apply each local modification listed in the file headers. The patches
   are small and self-contained — re-applying by hand is fast.
4. Re-run the Phase A smoke test (load checkpoint, embed a few tracks,
   compare cosine similarities against the previous run). Any meaningful
   numerical drift is a signal that the model definition has changed
   non-trivially upstream.
5. Bump the commit SHA + date in this file's "Source" section and in
   each vendored `.py` header.

## Why vendored and not submoduled / pip-installed

- CLaMP 3 has no PyPI package and no installable Python package layout
  (scripts use cwd-relative imports, no `pyproject.toml`).
- The two patches above (`load_audio_bytes` for in-memory bytes input;
  module-renaming for snake_case + relative imports) are required for
  integration, not just convenience — a read-only submodule could not host
  them without us forking upstream first, which would make provenance
  *more* opaque, not less.
- Build-time downloading from upstream (e.g. fetching a tarball in the
  Dockerfile) was considered but rejected because the patches would have
  to live as separate `.patch` files that drift silently if upstream
  changes adjacent lines. In-tree vendored files diff cleanly against
  upstream and patches live as normal git history.
