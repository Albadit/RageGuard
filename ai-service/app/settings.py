"""Service configuration from environment variables."""

from __future__ import annotations

import os
from dataclasses import dataclass

DEFAULT_MODEL_ID = "Dpngtm/wav2vec2-emotion-recognition"


def _env_int(name: str, default: int) -> int:
    raw = os.getenv(name, "").strip()
    if not raw:
        return default
    try:
        value = int(raw)
    except ValueError as exc:
        raise ValueError(f"{name} must be an integer, got {raw!r}") from exc
    if value <= 0:
        raise ValueError(f"{name} must be positive, got {value}")
    return value


@dataclass(frozen=True)
class Settings:
    # Hugging Face model id or local path.
    model_id: str = DEFAULT_MODEL_ID
    # "cpu", "cuda", "cuda:0", ... or None to pick automatically.
    device: str | None = None
    # Largest accepted upload. 1 MiB holds ~30 s of 16 kHz mono PCM, and Starlette keeps uploads
    # of this size in memory rather than spooling them to disk.
    max_upload_bytes: int = 1024 * 1024
    # Limit PyTorch CPU threads (None = PyTorch default).
    torch_threads: int | None = None
    log_level: str = "info"

    @classmethod
    def from_env(cls) -> Settings:
        threads = os.getenv("TORCH_THREADS", "").strip()
        return cls(
            model_id=os.getenv("MODEL_ID", "").strip() or DEFAULT_MODEL_ID,
            device=os.getenv("DEVICE", "").strip() or None,
            max_upload_bytes=_env_int("MAX_UPLOAD_BYTES", cls.max_upload_bytes),
            torch_threads=_env_int("TORCH_THREADS", 1) if threads else None,
            log_level=(os.getenv("LOG_LEVEL", "").strip() or "info").lower(),
        )
