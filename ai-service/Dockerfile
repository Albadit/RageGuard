# syntax=docker/dockerfile:1
FROM python:3.12-slim

ENV PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1 \
    PIP_NO_CACHE_DIR=1 \
    PIP_DISABLE_PIP_VERSION_CHECK=1 \
    HF_HOME=/models

WORKDIR /srv

# CPU-only PyTorch keeps the image small. For NVIDIA GPUs, build with
#   --build-arg TORCH_INDEX_URL=https://download.pytorch.org/whl/cu128
# and run with `gpus: all` in docker-compose.yml.
ARG TORCH_INDEX_URL=https://download.pytorch.org/whl/cpu
RUN pip install torch --index-url "${TORCH_INDEX_URL}"

COPY requirements.txt .
RUN pip install -r requirements.txt

COPY app ./app

RUN useradd --system --uid 10001 --create-home rageguard \
    && mkdir -p /models \
    && chown rageguard /models

# Optionally bake the model into the image instead of downloading it on first start:
#   docker compose build --build-arg PRELOAD_MODEL=true
ARG PRELOAD_MODEL=false
ARG MODEL_ID=Dpngtm/wav2vec2-emotion-recognition
USER rageguard
RUN if [ "${PRELOAD_MODEL}" = "true" ]; then \
      python -c "from transformers import AutoFeatureExtractor, AutoModelForAudioClassification as M; AutoFeatureExtractor.from_pretrained('${MODEL_ID}'); M.from_pretrained('${MODEL_ID}')"; \
    fi

EXPOSE 8000

# Healthy only once the model has loaded (/health returns 503 while loading).
HEALTHCHECK --interval=15s --timeout=5s --start-period=300s --retries=4 \
    CMD python -c "import urllib.request, sys; urllib.request.urlopen('http://127.0.0.1:8000/health', timeout=4); sys.exit(0)"

CMD ["uvicorn", "app.main:app", "--host", "0.0.0.0", "--port", "8000"]
