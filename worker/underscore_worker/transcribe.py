"""Speech to text, a chapter at a time, with faster-whisper on the GPU.

The transcript is kept in the format the phone imports (see
`crates/bridge/src/transcript.rs`): `{"chapters": [{"index", "title",
"start", "end", "segments": [{"start", "end", "text"}]}]}`, times in
seconds from the start of the book."""
from __future__ import annotations

import json
from pathlib import Path
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from .servers import Audiobookshelf, Book

RATE = 16000


class Transcriber:
    def __init__(self, model: str, device: str):
        self.name = model
        self.device = device
        self._model = None

    def model(self):
        if self._model is None:
            # Torch's wheels carry cuDNN; loading torch first lets
            # faster-whisper's CTranslate2 find it.
            import torch  # noqa: F401
            from faster_whisper import WhisperModel

            compute = "int8_float16" if self.device == "cuda" else "int8"
            self._model = WhisperModel(self.name, device=self.device, compute_type=compute)
        return self._model

    def chapter(self, abs: Audiobookshelf, book: Book, start: float, end: float) -> list[dict]:
        """Segments for `start` to `end` of the book, across its files."""
        from . import audio

        out = []
        for f in book.files:
            lo, hi = max(start, f.start), min(end, f.start + f.duration)
            if hi - lo < 0.5:
                continue
            samples = audio.read(abs.local(book, f), lo - f.start, hi - f.start, RATE)
            segments, _ = self.model().transcribe(samples, language="en", vad_filter=True, beam_size=5)
            for s in segments:
                text = s.text.strip()
                if text:
                    out.append({"start": round(lo + s.start, 2), "end": round(lo + s.end, 2), "text": text})
        return out


class Transcript:
    """One book's transcript, filled in chapter by chapter and kept on disk."""

    def __init__(self, path: Path, book: Book, model: str):
        self.path = path
        self.data = {"model": model, "source": book.title, "chapters": []}
        if path.is_file():
            self.data = json.loads(path.read_text())
        self.by_index = {c["index"]: c for c in self.data["chapters"]}

    @staticmethod
    def adopt(path: Path, book: Book, given: bytes) -> "Transcript":
        """Take a transcript made before: the map server's, or one beside
        the book. Only the chapters it covers count as done, so a partial
        one is finished rather than taken as whole."""
        from .compose import chapters_of

        v = json.loads(given)
        t = Transcript(path, book, v.get("model") or "made before")
        parts = chapters_of(book)
        given_chapters = v.get("chapters") or []
        if given_chapters and all("index" in c for c in given_chapters):
            # Our own format: chapters by index, as this worker wrote them.
            for c in given_chapters:
                i = c["index"]
                if 0 <= i < len(parts) and c.get("segments") is not None:
                    p = parts[i]
                    t.put(i, p["title"], p["start"], p["end"], c["segments"])
            return t
        segs = []
        for c in given_chapters or [{"segments": v.get("segments") or []}]:
            segs += c.get("segments") or []
        for i, p in enumerate(parts):
            mine = [s for s in segs if p["start"] <= float(s["start"]) < p["end"]]
            if mine:
                t.put(i, p["title"], p["start"], p["end"], mine)
        return t

    def has(self, index: int) -> bool:
        return index in self.by_index

    def segments(self, index: int) -> list[dict]:
        return self.by_index[index]["segments"]

    def put(self, index: int, title: str, start: float, end: float, segments: list[dict]) -> None:
        self.by_index[index] = {"index": index, "title": title, "start": start, "end": end, "segments": segments}
        self.data["chapters"] = [self.by_index[i] for i in sorted(self.by_index)]
        self.path.parent.mkdir(parents=True, exist_ok=True)
        tmp = self.path.with_suffix(".tmp")
        tmp.write_text(json.dumps(self.data))
        tmp.rename(self.path)
