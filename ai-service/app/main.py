"""FastAPI application: ``POST /analyze`` and ``GET /health``."""

from __future__ import annotations

import logging
import threading
import time
from collections.abc import Callable
from contextlib import asynccontextmanager

from fastapi import FastAPI, File, HTTPException, Request, UploadFile
from fastapi.concurrency import run_in_threadpool
from fastapi.responses import JSONResponse

from .audio import TARGET_SAMPLE_RATE, AudioError, load_audio
from .model import EmotionClassifier, Wav2Vec2EmotionModel
from .settings import Settings

logger = logging.getLogger("rageguard.api")

# Multipart framing around the file (boundaries, headers) on top of the audio bytes.
MULTIPART_OVERHEAD = 16 * 1024


class ModelHolder:
    """Loads the model in a background thread so ``/health`` answers while it downloads."""

    def __init__(self, loader: Callable[[], EmotionClassifier]) -> None:
        self._loader = loader
        self._thread: threading.Thread | None = None
        self.model: EmotionClassifier | None = None
        self.error: str | None = None

    def start(self) -> None:
        if self._thread is None:
            self._thread = threading.Thread(target=self._load, name="model-loader", daemon=True)
            self._thread.start()

    def _load(self) -> None:
        try:
            self.model = self._loader()
        except Exception as exc:  # noqa: BLE001 - reported through /health
            logger.exception("model failed to load")
            self.error = f"{type(exc).__name__}: {exc}"

    @property
    def status(self) -> str:
        if self.model is not None:
            return "ok"
        return "error" if self.error else "loading"


def create_app(
    loader: Callable[[], EmotionClassifier] | None = None,
    settings: Settings | None = None,
) -> FastAPI:
    settings = settings or Settings.from_env()
    if loader is None:

        def loader() -> EmotionClassifier:
            return Wav2Vec2EmotionModel(settings.model_id, settings.device, settings.torch_threads)

    models = ModelHolder(loader)

    @asynccontextmanager
    async def lifespan(_: FastAPI):
        models.start()
        yield

    app = FastAPI(
        title="RageGuard AI service",
        version="0.1.0",
        description="Speech-emotion recognition for RageGuard. Audio is processed in memory and never stored.",
        lifespan=lifespan,
    )
    app.state.models = models
    app.state.settings = settings

    @app.get("/health")
    def health() -> JSONResponse:
        body: dict[str, object] = {"status": models.status, "model_loaded": models.model is not None}
        if models.error:
            body["error"] = models.error
        return JSONResponse(body, status_code=200 if models.model is not None else 503)

    @app.post("/analyze")
    async def analyze(request: Request, file: UploadFile = File(...)) -> dict[str, object]:
        model = models.model
        if model is None:
            detail = f"Model failed to load: {models.error}" if models.error else "Model is still loading"
            raise HTTPException(status_code=503, detail=detail)

        declared = request.headers.get("content-length")
        if declared and declared.isdigit() and int(declared) > settings.max_upload_bytes + MULTIPART_OVERHEAD:
            raise HTTPException(status_code=413, detail="Upload too large")

        try:
            data = await file.read(settings.max_upload_bytes + 1)
        finally:
            # Releases the upload buffer (and any spooled temp file) immediately.
            await file.close()
        if len(data) > settings.max_upload_bytes:
            raise HTTPException(status_code=413, detail="Upload too large")

        try:
            audio = await run_in_threadpool(load_audio, data)
        except AudioError as exc:
            raise HTTPException(status_code=422, detail=str(exc)) from exc
        finally:
            del data

        started = time.perf_counter()
        try:
            prediction = await run_in_threadpool(model.predict, audio, TARGET_SAMPLE_RATE)
        except Exception as exc:
            logger.exception("inference failed")
            raise HTTPException(status_code=500, detail="Inference failed") from exc
        elapsed_ms = (time.perf_counter() - started) * 1000
        duration = len(audio) / TARGET_SAMPLE_RATE
        del audio

        logger.info(
            "emotion=%s confidence=%.3f duration=%.2fs inference_ms=%.0f",
            prediction.emotion,
            prediction.confidence,
            duration,
            elapsed_ms,
        )
        return {
            "emotion": prediction.emotion,
            "confidence": prediction.confidence,
            "scores": prediction.scores,
        }

    return app


def _configure_logging() -> None:
    level = Settings.from_env().log_level.upper()
    logging.basicConfig(
        level=getattr(logging, level, logging.INFO),
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
    )
    # huggingface_hub logs every metadata request at INFO.
    logging.getLogger("httpx").setLevel(logging.WARNING)


_configure_logging()
app = create_app()
