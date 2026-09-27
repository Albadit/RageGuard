"""Decoding and normalising uploaded audio.

Everything happens in memory: uploads are decoded from a byte buffer and never written to disk.
"""

from __future__ import annotations

import io

import numpy as np
import soundfile as sf
import soxr

# wav2vec2 models are trained on 16 kHz audio.
TARGET_SAMPLE_RATE = 16_000
# Shorter clips carry too little prosody to classify meaningfully.
MIN_DURATION_S = 0.5
# RageGuard sends 2–5 s segments; anything far longer is a misuse of the endpoint.
MAX_DURATION_S = 30.0


class AudioError(ValueError):
    """The upload is not decodable audio or is unsuitable for inference."""


def load_audio(data: bytes, target_rate: int = TARGET_SAMPLE_RATE) -> np.ndarray:
    """Decodes WAV/FLAC/OGG bytes into mono float32 samples at ``target_rate``."""
    if not data:
        raise AudioError("empty upload")

    try:
        with sf.SoundFile(io.BytesIO(data)) as audio_file:
            rate = audio_file.samplerate
            if rate <= 0:
                raise AudioError("invalid sample rate")
            if audio_file.frames / rate > MAX_DURATION_S:
                raise AudioError(
                    f"audio is {audio_file.frames / rate:.1f} s long; the maximum is {MAX_DURATION_S:.0f} s"
                )
            samples = audio_file.read(dtype="float32", always_2d=True)
    except AudioError:
        raise
    except (RuntimeError, TypeError, ValueError) as exc:
        # soundfile raises LibsndfileError (a RuntimeError) for unknown formats and corrupted data.
        raise AudioError(f"unsupported or corrupted audio: {exc}") from exc

    if samples.size == 0:
        raise AudioError("audio contains no samples")

    mono = samples.mean(axis=1, dtype=np.float32)
    if rate != target_rate:
        mono = soxr.resample(mono, rate, target_rate, quality="HQ").astype(np.float32, copy=False)

    duration = len(mono) / target_rate
    if duration < MIN_DURATION_S:
        raise AudioError(f"audio is {duration:.2f} s long; the minimum is {MIN_DURATION_S} s")
    if not np.isfinite(mono).all():
        raise AudioError("audio contains invalid (NaN/inf) samples")
    return mono
