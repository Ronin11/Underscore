"""The music index: a CLAP embedding for each song in the listener's
library, so a scene's mood, in words, can be matched to music by sound.

Each song is heard at three ten-second windows, a quarter, half, and
three quarters through, and the three are averaged. The index keeps the
song's tags with it, so a cue can name the track by artist and title for
the phone to find. Embeddings are kept; the audio is not."""
import json
from pathlib import Path

import numpy as np

from . import audio
from .servers import Song, Subsonic

RATE = 48000
WINDOW = 10.0
# What singing sounds like, against what instrumental music sounds like:
# underscoring speech wants no voice competing with the narrator's.
VOICE = "a person singing lyrics, vocals"
NO_VOICE = "instrumental music with no vocals"


class Clap:
    def __init__(self, name: str, device: str):
        self.name = name
        self.device = device
        self._m = None

    def _load(self):
        if self._m is None:
            import torch
            from transformers import ClapModel, ClapProcessor

            proc = ClapProcessor.from_pretrained(self.name)
            model = ClapModel.from_pretrained(self.name).eval().to(self.device)
            self._m = (proc, model, torch)
        return self._m

    @staticmethod
    def _unit(x):
        x = getattr(x, "pooler_output", x)
        return x / x.norm(dim=-1, keepdim=True)

    def text(self, texts: list[str]) -> np.ndarray:
        proc, model, torch = self._load()
        with torch.no_grad():
            inputs = proc(text=texts, return_tensors="pt", padding=True).to(self.device)
            return self._unit(model.get_text_features(**inputs)).cpu().numpy()

    def sound(self, windows: list[np.ndarray]) -> np.ndarray:
        proc, model, torch = self._load()
        with torch.no_grad():
            inputs = proc(audio=windows, sampling_rate=RATE, return_tensors="pt").to(self.device)
            e = self._unit(model.get_audio_features(**inputs)).mean(dim=0, keepdim=True)
            return (e / e.norm(dim=-1, keepdim=True)).cpu().numpy()[0]


class Index:
    """Songs and their embeddings, on disk under `dir`."""

    def __init__(self, dir: Path, clap: Clap):
        self.dir = dir
        self.clap = clap
        self.songs: list[dict] = []
        self.emb = np.zeros((0, 512), dtype=np.float32)
        self.voice = np.zeros(0, dtype=np.float32)
        meta = dir / "index.json"
        if meta.is_file():
            v = json.loads(meta.read_text())
            if v.get("model") == clap.name:
                self.songs = v["songs"]
                self.emb = np.load(dir / "embeddings.npy")
                self._score_voice()

    def _score_voice(self) -> None:
        if not len(self.songs):
            return
        t = self.clap.text([VOICE, NO_VOICE])
        sims = self.emb @ t.T
        self.voice = np.clip(sims[:, 0] - sims[:, 1], 0, None).astype(np.float32)

    def _save(self) -> None:
        self.dir.mkdir(parents=True, exist_ok=True)
        np.save(self.dir / "embeddings.npy.tmp.npy", self.emb)
        (self.dir / "embeddings.npy.tmp.npy").rename(self.dir / "embeddings.npy")
        tmp = self.dir / "index.json.tmp"
        tmp.write_text(json.dumps({"model": self.clap.name, "songs": self.songs}))
        tmp.rename(self.dir / "index.json")

    def refresh(self, sub: Subsonic, only: tuple, log) -> None:
        """Add songs the server has that the index lacks, and drop songs
        it no longer has."""
        songs = [s for s in sub.songs() if s.seconds >= 20 and (not only or s.path.startswith(only))]
        have = {s["id"]: i for i, s in enumerate(self.songs)}
        live = {s.id for s in songs}
        keep = [i for i, s in enumerate(self.songs) if s["id"] in live]
        if len(keep) != len(self.songs):
            self.songs = [self.songs[i] for i in keep]
            self.emb = self.emb[keep]
        new = [s for s in songs if s.id not in have]
        if not new:
            self._score_voice()
            return
        log(f"indexing {len(new)} new songs")
        rows, metas = [], []
        for n, s in enumerate(new, 1):
            try:
                rows.append(self._embed(sub, s))
                metas.append(
                    {"id": s.id, "artist": s.artist, "title": s.title, "album": s.album, "path": s.path,
                     "seconds": s.seconds, "genre": s.genre}
                )
            except Exception as e:  # one unreadable file should not stop the rest
                log(f"skipping {s.path}: {e}")
            if n % 100 == 0 or n == len(new):
                if rows:
                    self.emb = np.vstack([self.emb, np.stack(rows)]) if len(self.emb) else np.stack(rows)
                    self.songs += metas
                    rows, metas = [], []
                    self._save()
                log(f"indexed {n} of {len(new)}")
        self._save()
        self._score_voice()

    def _embed(self, sub: Subsonic, s: Song) -> np.ndarray:
        path = sub.local(s)
        try:
            total = s.seconds or audio.length(path)
            windows = []
            for at in (0.25, 0.5, 0.75):
                start = max(0.0, min(total * at, total - WINDOW))
                w = audio.read(path, start, start + WINDOW, RATE)
                if len(w) > RATE:
                    windows.append(w)
            if not windows:
                raise ValueError("no audio")
            return self.clap.sound(windows).astype(np.float32)
        finally:
            # A song downloaded to be heard is not kept.
            if sub.root is None or not str(path).startswith(str(sub.root)):
                path.unlink(missing_ok=True)

    def find(self, artist: str, title: str, everything: list[Song]) -> Song | None:
        """A song the book names, by its tags, among all the server's songs."""
        a, t = _norm(artist), _norm(title)
        for s in everything:
            if _norm(s.title) == t and (not a or a in _norm(s.artist) or _norm(s.artist) in a):
                return s
        return None


def _norm(s: str) -> str:
    return "".join(c for c in s.lower() if c.isalnum())
