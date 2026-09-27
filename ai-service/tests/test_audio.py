from __future__ import annotations

import numpy as np
import pytest

from app.audio import AudioError, load_audio

from .conftest import make_audio


def rising_zero_crossings_per_second(samples: np.ndarray, rate: int) -> float:
    crossings = np.count_nonzero((samples[:-1] < 0) & (samples[1:] >= 0))
    return crossings / (len(samples) / rate)


def test_16k_mono_passes_through():
    audio = load_audio(make_audio(seconds=2.0))
    assert audio.dtype == np.float32
    assert audio.shape == (32_000,)
    assert np.abs(audio).max() == pytest.approx(0.5, abs=0.01)


def test_stereo_is_downmixed():
    audio = load_audio(make_audio(channels=2))
    assert audio.ndim == 1
    assert np.abs(audio).max() == pytest.approx(0.5, abs=0.01)


def test_48k_is_resampled_preserving_pitch():
    audio = load_audio(make_audio(rate=48_000, freq=1_000.0))
    assert len(audio) == 48_000
    body = audio[500:-500]
    assert rising_zero_crossings_per_second(body, 16_000) == pytest.approx(1_000, abs=10)


def test_float_wav_is_supported():
    audio = load_audio(make_audio(subtype="FLOAT"))
    assert len(audio) == 48_000


@pytest.mark.parametrize("data", [b"", b"not audio", b"RIFF\x24\x00\x00\x00WAVEfmt "])
def test_invalid_data_raises(data):
    with pytest.raises(AudioError):
        load_audio(data)


def test_too_short_raises():
    with pytest.raises(AudioError, match="minimum"):
        load_audio(make_audio(seconds=0.3))


def test_too_long_raises():
    with pytest.raises(AudioError, match="maximum"):
        load_audio(make_audio(seconds=40, rate=8_000))
