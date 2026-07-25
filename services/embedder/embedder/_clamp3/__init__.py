"""Vendored CLaMP 3 inference code. See VENDORED.md for provenance.

This package is private to the embedder service — its API surface is
intentionally narrow: re-export only the classes the `Clamp3Embedder`
backend needs to construct the model and feed it audio.

Citation:
  Wu, S., et al. (2025). "CLaMP 3: Universal Music Information Retrieval
  Across Unaligned Modalities and Unseen Languages." Findings of ACL 2025.
  https://aclanthology.org/2025.findings-acl.133/
"""

from .audio_io import load_audio_bytes
from .feature_extractor import HuBERTFeature
from .model import CLaMP3Model
from .musichubert_config import MusicHubertConfig
from .musichubert_model import MusicHubertModel

__all__ = [
    "CLaMP3Model",
    "HuBERTFeature",
    "MusicHubertConfig",
    "MusicHubertModel",
    "load_audio_bytes",
]
