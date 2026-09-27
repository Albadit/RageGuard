"""Speech-emotion model wrapper.

PyTorch and Transformers are imported lazily so the rest of the service (and its tests) work
without them installed.
"""

from __future__ import annotations

import logging
import threading
import time
from dataclasses import dataclass, field
from typing import Protocol

import numpy as np

logger = logging.getLogger("rageguard.model")

# Canonical labels RageGuard understands; model-specific spellings map onto these.
CANONICAL_LABELS = ("Angry", "Disgust", "Fear", "Happy", "Neutral", "Sad", "Surprise")
LABEL_ALIASES = {
    "angry": "Angry",
    "anger": "Angry",
    "ang": "Angry",
    "disgust": "Disgust",
    "disgusted": "Disgust",
    "dis": "Disgust",
    "fear": "Fear",
    "fearful": "Fear",
    "fea": "Fear",
    "happy": "Happy",
    "happiness": "Happy",
    "hap": "Happy",
    "joy": "Happy",
    "neutral": "Neutral",
    "neu": "Neutral",
    # Dpngtm/wav2vec2-emotion-recognition has no "neutral" class; its "calm" class plays that role.
    "calm": "Neutral",
    "sad": "Sad",
    "sadness": "Sad",
    "surprise": "Surprise",
    "surprised": "Surprise",
    "sur": "Surprise",
}


def normalize_label(label: str) -> str:
    """Maps a model label (``"fearful"``, ``"ang"``, ...) to RageGuard's canonical name."""
    cleaned = label.strip()
    return LABEL_ALIASES.get(cleaned.lower(), cleaned.title())


@dataclass(frozen=True)
class Prediction:
    emotion: str
    confidence: float
    scores: dict[str, float] = field(default_factory=dict)


def prediction_from_probabilities(labels: list[str], probabilities: np.ndarray) -> Prediction:
    """Builds a :class:`Prediction` from raw model labels and softmax probabilities.

    Labels that normalise to the same canonical name have their probabilities summed.
    """
    if len(labels) != len(probabilities):
        raise ValueError(f"{len(labels)} labels but {len(probabilities)} probabilities")
    scores: dict[str, float] = {}
    for label, probability in zip(labels, probabilities, strict=True):
        name = normalize_label(label)
        scores[name] = scores.get(name, 0.0) + float(probability)
    ordered = dict(sorted(scores.items(), key=lambda item: item[1], reverse=True))
    emotion, confidence = next(iter(ordered.items()))
    return Prediction(
        emotion=emotion,
        confidence=round(min(confidence, 1.0), 4),
        scores={name: round(min(value, 1.0), 4) for name, value in ordered.items()},
    )


class EmotionClassifier(Protocol):
    def predict(self, audio: np.ndarray, sample_rate: int) -> Prediction: ...


class Wav2Vec2EmotionModel:
    """Hugging Face audio-classification model (wav2vec2 or compatible)."""

    def __init__(
        self, model_id: str, device: str | None = None, torch_threads: int | None = None
    ) -> None:
        import torch
        from transformers import AutoFeatureExtractor, AutoModelForAudioClassification

        if torch_threads:
            torch.set_num_threads(torch_threads)
        self.device = torch.device(device or ("cuda" if torch.cuda.is_available() else "cpu"))

        started = time.perf_counter()
        logger.info("loading model %s on %s", model_id, self.device)
        self.extractor = AutoFeatureExtractor.from_pretrained(model_id)
        self.model = AutoModelForAudioClassification.from_pretrained(model_id)
        self.model.to(self.device)
        self.model.eval()
        config = self.model.config
        self.labels = [config.id2label[i] for i in range(config.num_labels)]
        self.expected_rate = int(getattr(self.extractor, "sampling_rate", 16_000))
        # One inference at a time keeps memory bounded and avoids oversubscribing CPU threads.
        self._lock = threading.Lock()
        logger.info(
            "model ready in %.1f s; labels: %s",
            time.perf_counter() - started,
            ", ".join(f"{raw}->{normalize_label(raw)}" for raw in self.labels),
        )

    def predict(self, audio: np.ndarray, sample_rate: int) -> Prediction:
        import torch

        if sample_rate != self.expected_rate:
            raise ValueError(f"model expects {self.expected_rate} Hz audio, got {sample_rate} Hz")
        inputs = self.extractor(audio, sampling_rate=sample_rate, return_tensors="pt")
        inputs = {name: tensor.to(self.device) for name, tensor in inputs.items()}
        with self._lock, torch.inference_mode():
            logits = self.model(**inputs).logits[0]
        probabilities = torch.softmax(logits.float(), dim=-1).cpu().numpy()
        return prediction_from_probabilities(self.labels, probabilities)
