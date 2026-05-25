"""Text embedding infrastructure for Bonsai ML.

EV1-3 T1: TextEmbedder wrapping sentence-transformers (default),
Ollama, or OpenAI. Used by syslog_embedding_worker.py and
config_embedding_worker.py.

Model selection:
  - sentence-transformers all-MiniLM-L6-v2 (384 dims, 22MB) — default.
    Offline, no API key, fast on CPU (~200 samples/sec).
  - nomic-embed-text via Ollama (768 dims) — if Ollama sidecar is running.
  - OpenAI text-embedding-3-small (1536 dims) — optional, API-key gated.
    Only for investigation prompts, never for bulk syslog (cost prohibitive).

Configuration via EmbeddingConfig dataclass or bonsai.toml [ml.embedding] section.
"""
from __future__ import annotations

import logging
import os
import time
from dataclasses import dataclass, field
from typing import Any, Optional

import numpy as np

log = logging.getLogger(__name__)

DEFAULT_MODEL = "all-MiniLM-L6-v2"
DEFAULT_DIM = 384
DEFAULT_BATCH_SIZE = 64
DEFAULT_MAX_TEXT_LENGTH = 256


@dataclass
class EmbeddingConfig:
    """Configuration for the text embedding backend."""
    model_name: str = DEFAULT_MODEL
    dim: int = DEFAULT_DIM
    batch_size: int = DEFAULT_BATCH_SIZE
    max_text_length: int = DEFAULT_MAX_TEXT_LENGTH
    backend: str = "sentence_transformers"
    ollama_url: str = "http://localhost:11434"
    openai_api_key_env: str = "OPENAI_API_KEY"
    openai_model: str = "text-embedding-3-small"


def load_from_config(config: dict[str, Any]) -> "TextEmbedder":
    """Construct a TextEmbedder from a config dict (from bonsai.toml or DB)."""
    emb_cfg = EmbeddingConfig(
        model_name=config.get("model_name", DEFAULT_MODEL),
        dim=config.get("dim", DEFAULT_DIM),
        batch_size=config.get("batch_size", DEFAULT_BATCH_SIZE),
        max_text_length=config.get("max_text_length", DEFAULT_MAX_TEXT_LENGTH),
        backend=config.get("backend", "sentence_transformers"),
        ollama_url=config.get("ollama_url", "http://localhost:11434"),
        openai_api_key_env=config.get("openai_api_key_env", "OPENAI_API_KEY"),
        openai_model=config.get("openai_model", "text-embedding-3-small"),
    )
    return TextEmbedder(emb_cfg)


class TextEmbedder:
    """Unified text embedding interface supporting multiple backends.

    Lazy-initialises the underlying model on first call to avoid import
    overhead and allow the module to load in environments without ML deps.
    """

    def __init__(self, config: EmbeddingConfig | None = None) -> None:
        self.config = config or EmbeddingConfig()
        self._model: Any = None

    @property
    def dim(self) -> int:
        return self.config.dim

    @property
    def model_name(self) -> str:
        return self.config.model_name

    def embed_batch(self, texts: list[str]) -> np.ndarray:
        """Embed a batch of text strings.

        Args:
            texts: List of raw text strings (syslog messages, config snippets, etc.)

        Returns:
            float32 numpy array of shape (N, dim). Never raises on empty input.
        """
        if not texts:
            return np.zeros((0, self.config.dim), dtype=np.float32)

        truncated = [t[: self.config.max_text_length] for t in texts]

        backend = self.config.backend
        if backend == "sentence_transformers":
            return self._embed_sentence_transformers(truncated)
        elif backend == "ollama":
            return self._embed_ollama(truncated)
        elif backend == "openai":
            return self._embed_openai(truncated)
        else:
            log.warning("Unknown embedding backend %r, falling back to sentence_transformers", backend)
            return self._embed_sentence_transformers(truncated)

    def embed_single(self, text: str) -> np.ndarray:
        """Embed a single text string. Returns shape (dim,)."""
        return self.embed_batch([text])[0]

    def _embed_sentence_transformers(self, texts: list[str]) -> np.ndarray:
        try:
            from sentence_transformers import SentenceTransformer
        except ImportError as exc:
            raise RuntimeError(
                "sentence-transformers not installed. "
                "Run: pip install sentence-transformers"
            ) from exc

        if self._model is None:
            log.info("Loading sentence-transformer model: %s", self.config.model_name)
            self._model = SentenceTransformer(self.config.model_name)

        results = []
        batch_size = self.config.batch_size
        for i in range(0, len(texts), batch_size):
            batch = texts[i: i + batch_size]
            embeddings = self._model.encode(
                batch,
                convert_to_numpy=True,
                show_progress_bar=False,
                normalize_embeddings=True,
            )
            results.append(embeddings.astype(np.float32))

        return np.vstack(results)

    def _embed_ollama(self, texts: list[str]) -> np.ndarray:
        try:
            import requests
        except ImportError as exc:
            raise RuntimeError("requests not installed") from exc

        results: list[np.ndarray] = []
        for text in texts:
            try:
                resp = requests.post(
                    f"{self.config.ollama_url}/api/embeddings",
                    json={"model": self.config.model_name, "prompt": text},
                    timeout=15,
                )
                resp.raise_for_status()
                vec = np.array(resp.json()["embedding"], dtype=np.float32)
                norm = np.linalg.norm(vec)
                if norm > 0:
                    vec = vec / norm
                results.append(vec)
            except Exception as exc:
                log.warning("Ollama embedding failed for text (len=%d): %s", len(text), exc)
                results.append(np.zeros(self.config.dim, dtype=np.float32))

        return np.array(results, dtype=np.float32)

    def _embed_openai(self, texts: list[str]) -> np.ndarray:
        api_key = os.environ.get(self.config.openai_api_key_env)
        if not api_key:
            raise RuntimeError(
                f"OpenAI API key not set (env var: {self.config.openai_api_key_env})"
            )
        try:
            import openai
        except ImportError as exc:
            raise RuntimeError("openai package not installed: pip install openai") from exc

        client = openai.OpenAI(api_key=api_key)
        results: list[np.ndarray] = []
        batch_size = min(self.config.batch_size, 100)

        for i in range(0, len(texts), batch_size):
            batch = texts[i: i + batch_size]
            try:
                response = client.embeddings.create(
                    model=self.config.openai_model,
                    input=batch,
                )
                for item in response.data:
                    vec = np.array(item.embedding, dtype=np.float32)
                    norm = np.linalg.norm(vec)
                    if norm > 0:
                        vec = vec / norm
                    results.append(vec)
            except Exception as exc:
                log.error("OpenAI embedding batch failed: %s", exc)
                for _ in batch:
                    results.append(np.zeros(self.config.dim, dtype=np.float32))

        return np.array(results, dtype=np.float32)
