"""
Vendored from CLaMP 3 (https://github.com/sanderwood/clamp3).
Upstream path:   code/config.py
Upstream commit: 9016d2b0c8d12d1aa79c2e0ab201e6822bdc83a8
License:         MIT (see ./LICENSES/MIT-LICENSE)

Local modifications:
- 2026-05-30: Slimmed to inference-only fields. Dropped training-data paths,
  wandb keys, learning rates, batch sizes, epochs, weight-path templates,
  and all M3-training fields except those that the audio-side model
  classes still reference (PATCH_SIZE/LENGTH and M3_HIDDEN_SIZE are read
  by M3PatchEncoder, which CLaMP3Model instantiates as self.symbolic_model
  even though we never call forward() on it).
"""

# -------------------- Patch geometry (used by M3PatchEncoder) ---------------
# CLaMP3Model.__init__ instantiates an M3PatchEncoder as self.symbolic_model,
# which reads these. Inference for audio never touches that path, but the
# nn.Module has to be constructable for state_dict loading to succeed.
PATCH_SIZE = 64
PATCH_LENGTH = 512
PATCH_NUM_LAYERS = 12
M3_HIDDEN_SIZE = 768

# -------------------- CLaMP 3 audio + text encoder shapes -------------------
CLAMP3_HIDDEN_SIZE = 768                    # shared projection dim
TEXT_MODEL_NAME = "FacebookAI/xlm-roberta-base"
MAX_TEXT_LENGTH = 128                       # max tokens for text branch
AUDIO_HIDDEN_SIZE = 768                     # MERT-feature transformer hidden dim
AUDIO_NUM_LAYERS = 12                       # MERT-feature transformer depth
MAX_AUDIO_LENGTH = 128                      # max MERT-feature tokens (≈ 640 s)
LOGIT_SCALE = 1                             # referenced by forward(); set by training
