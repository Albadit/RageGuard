from __future__ import annotations

import threading

import numpy as np
from fastapi.testclient import TestClient

from app.main import create_app
from app.settings import Settings

from .conftest import NEUTRAL, FakeModel, make_audio, wait_for


def upload(client: TestClient, data: bytes, filename: str = "audio.wav", mime: str = "audio/wav"):
    return client.post("/analyze", files={"file": (filename, data, mime)})


def test_health_reports_loaded_model(client):
    response = client.get("/health")
    assert response.status_code == 200
    assert response.json() == {"status": "ok", "model_loaded": True}


def test_analyze_returns_emotion_json(client, fake_model):
    response = upload(client, make_audio())
    assert response.status_code == 200
    body = response.json()
    assert body["emotion"] == "Angry"
    assert body["confidence"] == 0.91
    assert body["scores"] == {"Angry": 0.91, "Neutral": 0.04, "Sad": 0.02}

    audio, rate = fake_model.calls[0]
    assert rate == 16_000
    assert audio.dtype == np.float32
    assert audio.ndim == 1
    assert len(audio) == 48_000


def test_neutral_prediction(fake_model, client):
    fake_model.prediction = NEUTRAL
    body = upload(client, make_audio()).json()
    assert body["emotion"] == "Neutral"
    assert body["confidence"] == 0.92


def test_discord_rate_stereo_is_converted(client, fake_model):
    response = upload(client, make_audio(rate=48_000, channels=2))
    assert response.status_code == 200
    audio, rate = fake_model.calls[0]
    assert rate == 16_000
    assert len(audio) == 48_000


def test_flac_is_accepted(client):
    response = upload(client, make_audio(fmt="FLAC"), "audio.flac", "audio/flac")
    assert response.status_code == 200


def test_corrupted_audio_is_rejected(client, fake_model):
    response = upload(client, b"RIFF\x00\x00\x00\x00WAVEgarbage" * 10)
    assert response.status_code == 422
    assert "unsupported or corrupted" in response.json()["detail"]
    assert fake_model.calls == []


def test_non_audio_is_rejected(client):
    assert upload(client, b"hello world", "notes.txt", "text/plain").status_code == 422


def test_empty_upload_is_rejected(client):
    response = upload(client, b"")
    assert response.status_code == 422


def test_too_short_audio_is_rejected(client):
    response = upload(client, make_audio(seconds=0.2))
    assert response.status_code == 422
    assert "minimum" in response.json()["detail"]


def test_too_long_audio_is_rejected(client):
    response = upload(client, make_audio(seconds=31, rate=8_000))
    assert response.status_code == 422
    assert "maximum" in response.json()["detail"]


def test_oversized_upload_is_rejected(fake_model):
    app = create_app(loader=lambda: fake_model, settings=Settings(max_upload_bytes=10_000))
    with TestClient(app) as client:
        wait_for(lambda: app.state.models.model is not None)
        response = upload(client, make_audio(seconds=3))
        assert response.status_code == 413
    assert fake_model.calls == []


def test_missing_file_field_is_a_validation_error(client):
    assert client.post("/analyze").status_code == 422


def test_requests_while_loading_get_503():
    release = threading.Event()

    def slow_loader():
        release.wait(5)
        return FakeModel()

    app = create_app(loader=slow_loader, settings=Settings())
    with TestClient(app) as client:
        health = client.get("/health")
        assert health.status_code == 503
        assert health.json() == {"status": "loading", "model_loaded": False}

        response = upload(client, make_audio())
        assert response.status_code == 503
        assert response.json()["detail"] == "Model is still loading"

        release.set()
        wait_for(lambda: app.state.models.model is not None)
        assert client.get("/health").status_code == 200


def test_load_failure_is_reported():
    def broken_loader():
        raise OSError("model not found")

    app = create_app(loader=broken_loader, settings=Settings())
    with TestClient(app) as client:
        wait_for(lambda: app.state.models.error is not None)
        health = client.get("/health")
        assert health.status_code == 503
        assert health.json()["status"] == "error"
        assert "model not found" in health.json()["error"]
        assert upload(client, make_audio()).status_code == 503


def test_inference_errors_become_500(fake_model, client):
    def explode(audio, rate):
        raise RuntimeError("CUDA out of memory")

    fake_model.predict = explode
    response = upload(client, make_audio())
    assert response.status_code == 500
    assert response.json()["detail"] == "Inference failed"
