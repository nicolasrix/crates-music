"""
Vendored from CLaMP 3 (https://github.com/sanderwood/clamp3).
Upstream path:   code/utils.py
Upstream commit: 9016d2b0c8d12d1aa79c2e0ab201e6822bdc83a8
License:         MIT (see ./LICENSES/MIT-LICENSE)

Local modifications:
- 2026-05-30: Slimmed to inference-only scope. Dropped:
  * ClipLoss (training contrastive loss, ~113 lines)
  * M3Patchilizer / M3TokenDecoder / M3Model (symbolic-music branch we never call)
  * split_data / mask_patches / remove_instrument_info (training utilities)
  * horovod and torch.distributed imports (used only by ClipLoss)
  * GPT2LMHeadModel / GPT2Config imports (used only by M3TokenDecoder)
- 2026-05-30: Trimmed CLaMP3Model:
  * Dropped self.loss_fn (ClipLoss) — inference path doesn't need contrastive loss.
  * Dropped set_trainable() method — training-only.
  * Dropped forward() method — inference calls get_audio_features / get_text_features
    directly (matches upstream's own extract_clamp3.py usage).
  * Defaulted load_m3=False — the unified `saas` checkpoint we load contains the
    M3 encoder weights baked in, so the side-load of a separate M3 checkpoint is
    unnecessary. This also lets us drop the M3Model import.
- 2026-05-30: Relative imports (`from .config import *`) — module is now a package.
- 2026-05-30: M3PatchEncoder retained verbatim. Not used at inference, but
  CLaMP3Model.__init__ instantiates it as self.symbolic_model so its nn.Module
  must exist for state_dict loading to align.
"""

import os
import torch
from torch.nn import functional as F
from transformers import AutoModel, BertModel, PreTrainedModel

from .config import (
    PATCH_SIZE,
    M3_HIDDEN_SIZE,
    CLAMP3_HIDDEN_SIZE,
    TEXT_MODEL_NAME,
)


class M3PatchEncoder(PreTrainedModel):
    def __init__(self, config):
        super(M3PatchEncoder, self).__init__(config)
        self.patch_embedding = torch.nn.Linear(PATCH_SIZE*128, M3_HIDDEN_SIZE)
        torch.nn.init.normal_(self.patch_embedding.weight, std=0.02)
        self.base = BertModel(config=config)
        self.pad_token_id = 0
        self.bos_token_id = 1
        self.eos_token_id = 2
        self.mask_token_id = 3

    def forward(self,
                input_patches, # [batch_size, seq_length, hidden_size]
                input_masks):  # [batch_size, seq_length]
        # Transform input_patches into embeddings
        input_patches = torch.nn.functional.one_hot(input_patches, num_classes=128)
        input_patches = input_patches.reshape(len(input_patches), -1, PATCH_SIZE*128).type(torch.FloatTensor)
        input_patches = self.patch_embedding(input_patches.to(self.device))

        # Apply BERT model to input_patches and input_masks
        return self.base(inputs_embeds=input_patches, attention_mask=input_masks)


class CLaMP3Model(PreTrainedModel):
    def __init__(self,
                 audio_config,
                 symbolic_config,
                 text_model_name=TEXT_MODEL_NAME,
                 hidden_size=CLAMP3_HIDDEN_SIZE,
                 load_m3=False):
        super(CLaMP3Model, self).__init__(symbolic_config)

        self.text_model = AutoModel.from_pretrained(text_model_name)  # Load the text model
        self.text_proj = torch.nn.Linear(self.text_model.config.hidden_size, hidden_size)
        torch.nn.init.normal_(self.text_proj.weight, std=0.02)

        self.symbolic_model = M3PatchEncoder(symbolic_config)
        self.symbolic_proj = torch.nn.Linear(M3_HIDDEN_SIZE, hidden_size)
        torch.nn.init.normal_(self.symbolic_proj.weight, std=0.02)

        self.audio_model = BertModel(audio_config)
        self.audio_proj = torch.nn.Linear(audio_config.hidden_size, hidden_size)
        torch.nn.init.normal_(self.audio_proj.weight, std=0.02)

        # Upstream's M3-checkpoint side-load is intentionally omitted; the unified
        # `saas` checkpoint already contains M3 encoder weights and we load it via
        # `model.load_state_dict(ckpt['model'])` in the caller.
        if load_m3:
            raise NotImplementedError(
                "load_m3=True path not vendored — the unified CLaMP 3 checkpoint "
                "already contains M3 weights; side-loading is unnecessary at inference."
            )

    def avg_pooling(self, input_features, input_masks):
        input_masks = input_masks.unsqueeze(-1).to(self.device)
        input_features = input_features * input_masks
        avg_pool = input_features.sum(dim=1) / input_masks.sum(dim=1)
        return avg_pool

    def get_text_features(self,
                          text_inputs,
                          text_masks,
                          get_global=False):
        text_features = self.text_model(text_inputs.to(self.device),
                                        attention_mask=text_masks.to(self.device))['last_hidden_state']
        if get_global:
            text_features = self.avg_pooling(text_features, text_masks)
            text_features = self.text_proj(text_features)
        return text_features

    def get_symbolic_features(self,
                              symbolic_inputs,
                              symbolic_masks,
                              get_global=False):
        symbolic_features = self.symbolic_model(symbolic_inputs.to(self.device),
                                                symbolic_masks.to(self.device))['last_hidden_state']
        if get_global:
            symbolic_features = self.avg_pooling(symbolic_features, symbolic_masks)
            symbolic_features = self.symbolic_proj(symbolic_features)
        return symbolic_features

    def get_audio_features(self,
                           audio_inputs,
                           audio_masks,
                           get_global=False):
        audio_features = self.audio_model(inputs_embeds=audio_inputs.to(self.device),
                                          attention_mask=audio_masks.to(self.device))['last_hidden_state']
        if get_global:
            audio_features = self.avg_pooling(audio_features, audio_masks)
            audio_features = self.audio_proj(audio_features)
        return audio_features
