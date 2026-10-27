"""Settings, from the environment. See worker.env.example."""
import os
from dataclasses import dataclass
from pathlib import Path


def _need(name: str) -> str:
    value = os.environ.get(name, "").strip()
    if not value:
        raise SystemExit(f"underscore-worker: set {name}")
    return value


def _get(name: str, default: str) -> str:
    return os.environ.get(name, "").strip() or default


@dataclass(frozen=True)
class Config:
    maps_url: str
    worker_token: str
    abs_url: str
    abs_token: str
    subsonic_url: str
    subsonic_user: str
    subsonic_password: str
    ollama_url: str
    ollama_model: str
    whisper_model: str
    clap_model: str
    device: str
    data: Path
    # Where the servers' folders are on this machine: Audiobookshelf's
    # paths start with a prefix such as /audiobooks.
    book_paths: dict
    # Navidrome's music folder on this machine; song paths are relative to it.
    music_root: Path | None
    # Only songs whose path starts with one of these are scored, such as
    # SideTrax/. Songs a book names are found anywhere.
    music_only: tuple

    @staticmethod
    def load() -> "Config":
        paths = {}
        for pair in _get("UNDERSCORE_BOOK_PATHS", "").split(";"):
            if "=" in pair:
                server, here = pair.split("=", 1)
                paths[server.strip().rstrip("/")] = here.strip().rstrip("/")
        root = _get("UNDERSCORE_MUSIC_ROOT", "")
        only = tuple(p.strip() for p in _get("UNDERSCORE_MUSIC_ONLY", "").split(",") if p.strip())
        return Config(
            maps_url=_need("UNDERSCORE_MAPS_URL").rstrip("/"),
            worker_token=_need("UNDERSCORE_WORKER_TOKEN"),
            abs_url=_need("UNDERSCORE_ABS_URL").rstrip("/"),
            abs_token=_need("UNDERSCORE_ABS_TOKEN"),
            subsonic_url=_need("UNDERSCORE_SUBSONIC_URL").rstrip("/"),
            subsonic_user=_need("UNDERSCORE_SUBSONIC_USER"),
            subsonic_password=_need("UNDERSCORE_SUBSONIC_PASSWORD"),
            ollama_url=_get("UNDERSCORE_OLLAMA_URL", "http://localhost:11434").rstrip("/"),
            ollama_model=_get("UNDERSCORE_OLLAMA_MODEL", "qwen3:30b-a3b"),
            whisper_model=_get("UNDERSCORE_WHISPER_MODEL", "distil-large-v3"),
            clap_model=_get("UNDERSCORE_CLAP_MODEL", "laion/larger_clap_general"),
            device=_get("UNDERSCORE_DEVICE", "cuda"),
            data=Path(_get("UNDERSCORE_WORKER_DATA", str(Path.home() / ".local/share/underscore-worker"))),
            book_paths=paths,
            music_root=Path(root) if root else None,
            music_only=only,
        )
