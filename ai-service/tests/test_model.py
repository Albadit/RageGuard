from __future__ import annotations

import os

import numpy as np
import pytest

from app.model import CANONICAL_LABELS, normalize_label, prediction_from_probabilities

from .conftest import make_audio

# Labels of Dpngtm/wav2vec2-emotion-recognition, in id order.
DPNGTM_LABELS = ["angry", "calm", "disgust", "fearful", "happy", "sad", "surprised"]


def test_model_labels_map_to_canonical_set():
    mapped = [normalize_label(label) for label in DPNGTM_LABELS]
    assert mapped == ["Angry", "Neutral", "Disgust", "Fear", "Happy", "Sad", "Surprise"]
    assert set(mapped) == set(CANONICAL_LABELS)


@pytest.mark.parametrize(
    ("raw", "expected"),
    [("ANG", "Angry"), (" neutral ", "Neutral"), ("surprise", "Surprise"), ("bored", "Bored")],
)
def test_label_normalisation(raw, expected):
    assert normalize_label(raw) == expected


def test_prediction_picks_highest_probability():
    probabilities = np.array([0.91, 0.04, 0.01, 0.01, 0.01, 0.01, 0.01], dtype=np.float32)
    prediction = prediction_from_probabilities(DPNGTM_LABELS, probabilities)
    assert prediction.emotion == "Angry"
    assert prediction.confidence == pytest.approx(0.91)
    assert list(prediction.scores)[0] == "Angry"
    assert prediction.scores["Neutral"] == pytest.approx(0.04)
    assert sum(prediction.scores.values()) == pytest.approx(1.0, abs=1e-3)


def test_duplicate_labels_are_merged():
    prediction = prediction_from_probabilities(
        ["neutral", "calm", "angry"], np.array([0.3, 0.3, 0.4])
    )
    assert prediction.emotion == "Neutral"
    assert prediction.confidence == pytest.approx(0.6)


def test_label_count_mismatch_is_an_error():
    with pytest.raises(ValueError):
        prediction_from_probabilities(["angry"], np.array([0.5, 0.5]))


@pytest.mark.real_model
@pytest.mark.skipif(
    os.getenv("RAGEGUARD_TEST_REAL_MODEL") != "1",
    reason="set RAGEGUARD_TEST_REAL_MODEL=1 to download and run the real model",
)
def test_real_model_end_to_end():
    pytest.importorskip("torch")
    from app.audio import load_audio
    from app.model import Wav2Vec2EmotionModel
    from app.settings import DEFAULT_MODEL_ID

    model = Wav2Vec2EmotionModel(DEFAULT_MODEL_ID)
    prediction = model.predict(load_audio(make_audio(seconds=3.0)), 16_000)
    assert prediction.emotion in CANONICAL_LABELS
    assert 0.0 <= prediction.confidence <= 1.0
    assert set(prediction.scores) == set(CANONICAL_LABELS)
