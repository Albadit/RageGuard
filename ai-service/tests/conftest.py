from __future__ import annotations

import io
import time

import numpy as np
import pytest
import soundfile as sf
from fastapi.testclient import TestClient

from app.main import create_app
from app.model import Prediction
from app.settings import Settings

ANGRY = Prediction("Angry", 0.91, {"Angry": 0.91, "Neutral": 0.04, "Sad": 0.02})
NEUTRAL = Prediction("Neutral", 0.92, {"Neutral": 0.92, "Angry": 0.03})


class FakeModel:
    """Stands in for the wav2vec2 model and records what it was given."""

    def __init__(self, prediction: Prediction = ANGRY) -> None:
        self.prediction = prediction
        self.calls: list[tuple[np.ndarray, int]] = []

    def predict(self, audio: np.ndarray, sample_rate: int) -> Prediction:
        self.calls.append((audio, sample_rate))
        return self.prediction


def make_audio(
    seconds: float = 3.0,
    rate: int = 16_000,
    channels: int = 1,
    freq: float = 220.0,
    amplitude: float = 0.5,
    fmt: str = "WAV",
    subtype: str = "PCM_16",
) -> bytes:
    t = np.arange(int(seconds * rate)) / rate
    tone = (amplitude * np.sin(2 * np.pi * freq * t)).astype(np.float32)
    data = np.repeat(tone[:, None], channels, axis=1)
    buffer = io.BytesIO()
    sf.write(buffer, data, rate, format=fmt, subtype=subtype)
    return buffer.getvalue()


def wait_for(predicate, timeout: float = 5.0) -> None:
    deadline = time.monotonic() + timeout
    while not predicate():
        if time.monotonic() > deadline:
            raise TimeoutError("condition not met")
        time.sleep(0.01)


@pytest.fixture
def fake_model() -> FakeModel:
    return FakeModel()


@pytest.fixture
def client(fake_model: FakeModel):
    app = create_app(loader=lambda: fake_model, settings=Settings())
    with TestClient(app) as test_client:
        wait_for(lambda: app.state.models.model is not None)
        yield test_client
