"""
Vendored from CLaMP 3 (https://github.com/sanderwood/clamp3).
Upstream path:   preprocessing/audio/hf_pretrains.py
Upstream commit: 9016d2b0c8d12d1aa79c2e0ab201e6822bdc83a8
License:         MIT (see ./LICENSES/MIT-LICENSE)

Local modifications:
- 2026-05-30: Dropped Data2vecFeature and SpeechHuBERTFeature classes.
  We use HuBERTFeature only (which wraps MERT). The two dropped classes
  are alternative feature-extractor frontends from upstream's research code
  that we never instantiate.
- 2026-05-30: Dropped unused imports: Data2VecAudioConfig, Data2VecAudioModel,
  HubertModel, AutoModel (all referenced only by the dropped classes).
- 2026-05-30: Changed module-relative imports from
  `from configuration_musichubert import …` / `from MusicHubert import …`
  to relative form `from .musichubert_config import …` /
  `from .musichubert_model import …` (vendored package layout).
"""

"""
Feature extractor for HuggingFace pretraiined transformer models
"""
import torch
import torch.nn as nn
from transformers import Wav2Vec2FeatureExtractor

from .musichubert_config import MusicHubertConfig
from .musichubert_model import MusicHubertModel


class AudioBERTFeature(nn.Module):
    """A huggingface AudioBERT model wrapper.
    """
    def __init__(
            self,
            pre_trained_folder,
            sample_rate,
            force_half=False,
            disable_backprop=True,
            processor_normalize=True,
        ) -> None:
        super().__init__()

    @torch.no_grad()
    def process_wav(self, waveform):
        # return the same shape
        return self.processor(
            waveform,
            return_tensors="pt",
            sampling_rate=self.sample_rate,
            padding=True).input_values[0]

    def forward(self, input_values, layer=-1, reduction="mean"):
        """Forward function.

        Let B = batch size, T = number of samples, H = hidden size
        Args:
            input_values (torch.Tensor):
                [B, T]
                Input audio tensor/array.

            layer (int/None, optional):
                return which layer's activation.
                supports pos/neg integer, and None.
                defaults to -1, the last layer.
                if layer is None, return all layers' activations.

            reduction (str, optional):
                how to reduce the features.
                supports "mean", "max", "none".
                defaults to "mean".
        Returns:
            torch.Tensor:
                [B, H]: layer!=None, reduction!="none"
                [L, B, H]: layer=None, reduction!="none"
                [B, T, H]: layer!=None, reduction="none"
                [L, B, T, H]: layer=None, reduction="none"
        """
        if not self.force_half:
            out = self.model(input_values, output_hidden_states=True).hidden_states
        else:
            out = self.model(input_values.half(), output_hidden_states=True).hidden_states
            out = [o.float() for o in out]

        if layer != None:
            out = out[layer] # [B, T, H]
        else:
            out = torch.stack(out) # [L, B, T, H]
        if reduction == "mean":
            return out.mean(-2)
        elif reduction == "max":
            return out.max(-2)[0]
        elif reduction == "none":
            return out
        else:
            raise NotImplementedError

    def sliding_window_forward(self, input_values, window_size_in_sample, stride_in_sample, layer=-1, reduction="mean", allow_non_full_window=True):
        """Use for loop to extract features from a long audio file.

        Args:
            input_values (torch.Tensor):
                [B, T]
                Input audio tensor/array.

            window_size_in_sample (int):
                window size in sample

            stride_in_sample (int):
                stride in sample

            layer (int/None, optional):
                return which layer's activation.
                supports pos/neg integer, and None.
                defaults to -1, the last layer.
                if layer is None, return all layers' activations.

            reduction (str, optional):
                how to reduce the features.
                supports "mean", "max", "none".
                defaults to "mean".

            allow_non_full_window (bool, optional):
                if True, allow the last window to be smaller than window_size_in_sample
        """
        B, T = input_values.shape
        out = []
        for i in range(0, T-window_size_in_sample+1, stride_in_sample):
            out.append(self.forward(input_values[:, i:i+window_size_in_sample], layer=layer, reduction=reduction))
        if allow_non_full_window and T % stride_in_sample != 0:
            out.append(self.forward(input_values[:, -window_size_in_sample:], layer=layer, reduction=reduction))
        return torch.stack(out)


class HuBERTFeature(AudioBERTFeature):
    """A huggingface HuBERT model wrapper.

    extract hubert features from a wav file
    There is [a processor from wav2vec2](https://github.com/huggingface/transformers/blob/main/src/transformers/models/wav2vec2/feature_extraction_wav2vec2.py).
    The processor can do padding and wavform normalization.
    """

    def __init__(
            self,
            pre_trained_folder,
            sample_rate,
            force_half=False,
            disable_backprop=True,
            processor_normalize=True,
        ) -> None:
        super().__init__(pre_trained_folder, sample_rate, force_half, disable_backprop, processor_normalize)
        self.sample_rate = sample_rate
        self.processor = Wav2Vec2FeatureExtractor(
            feature_size=1,
            sampling_rate=sample_rate,
            padding_value=0.0,
            return_attention_mask=True,
            do_normalize=processor_normalize,
        )

        if pre_trained_folder == 'random_model' or None:
            print('Using random HuBERT model.')
            self.model = MusicHubertModel(MusicHubertConfig())
        else:
            print(f'Loading HuBERT model from {pre_trained_folder}')
            self.model = MusicHubertModel.from_pretrained(pre_trained_folder)

        self.force_half = force_half
        if disable_backprop:
            self.model.eval()
            if self.force_half:
                self.model.half()

            for param in self.model.parameters():
                param.requires_grad = False
