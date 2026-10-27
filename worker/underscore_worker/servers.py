"""The three servers the worker talks to: the map server for jobs, the
Audiobookshelf server for books, and the Subsonic server (Navidrome) for
music. All three are the listener's own."""
import hashlib
import secrets
import shutil
from dataclasses import dataclass, field
from pathlib import Path

import requests

TIMEOUT = 60


class MapServer:
    def __init__(self, url: str, token: str):
        self.url = url
        self.s = requests.Session()
        self.s.headers["Authorization"] = f"Bearer {token}"

    def claim(self) -> dict | None:
        r = self.s.post(f"{self.url}/api/worker/claim", timeout=TIMEOUT)
        if r.status_code == 204:
            return None
        r.raise_for_status()
        return r.json()

    def progress(self, job: str, **news) -> dict:
        r = self.s.post(f"{self.url}/api/worker/jobs/{job}", json=news, timeout=TIMEOUT)
        if r.status_code >= 400:
            raise RuntimeError(f"map server: {r.status_code} {r.text}")
        return r.json()

    def kept(self, book: str, what: str) -> bytes | None:
        """What the server keeps for `book`: "transcript" or "voices", or None."""
        r = self.s.get(f"{self.url}/api/worker/books/{book}/{what}", timeout=TIMEOUT)
        if r.status_code == 404:
            return None
        r.raise_for_status()
        return r.content

    def put_voices(self, book: str, voices: dict) -> None:
        r = self.s.put(f"{self.url}/api/worker/books/{book}/voices", json=voices, timeout=TIMEOUT)
        r.raise_for_status()

    def put_transcript(self, book: str, transcript: dict) -> None:
        r = self.s.put(f"{self.url}/api/worker/books/{book}/transcript", json=transcript, timeout=TIMEOUT)
        r.raise_for_status()


@dataclass
class AudioFile:
    ino: str
    path: str
    ext: str
    start: float
    duration: float


@dataclass
class Book:
    id: str
    title: str
    author: str | None
    narrator: str | None
    asin: str | None
    isbn: str | None
    duration: float
    # (title, start, end) in seconds from the start of the book, in order.
    # Map cues count them from 1, as the phone does.
    chapters: list
    files: list
    # Files beside the book that are not audio, by name, to their ino.
    others: dict = field(default_factory=dict)


class Audiobookshelf:
    def __init__(self, url: str, token: str, paths: dict, cache: Path):
        self.url = url
        self.paths = paths
        self.cache = cache
        self.s = requests.Session()
        self.s.headers["Authorization"] = f"Bearer {token}"

    def book(self, id: str) -> Book:
        r = self.s.get(f"{self.url}/api/items/{id}", params={"expanded": 1}, timeout=TIMEOUT)
        r.raise_for_status()
        item = r.json()
        media = item.get("media") or {}
        meta = media.get("metadata") or {}
        files, at = [], 0.0
        for f in sorted(media.get("audioFiles") or [], key=lambda f: f.get("index") or 0):
            if f.get("exclude"):
                continue
            m = f.get("metadata") or {}
            d = float(f.get("duration") or 0)
            files.append(AudioFile(f["ino"], m.get("path") or "", (m.get("ext") or "").lstrip("."), at, d))
            at += d
        duration = float(media.get("duration") or at)
        chapters = [
            ((c.get("title") or "").strip(), float(c["start"]), float(c["end"]))
            for c in media.get("chapters") or []
        ]
        others = {}
        for lf in item.get("libraryFiles") or []:
            m = lf.get("metadata") or {}
            if lf.get("fileType") != "audio" and m.get("filename"):
                others[m["filename"]] = lf["ino"]
        return Book(
            id=id,
            title=meta.get("title") or "Untitled",
            author=meta.get("authorName") or None,
            narrator=meta.get("narratorName") or None,
            asin=meta.get("asin") or None,
            isbn=meta.get("isbn") or None,
            duration=duration,
            chapters=chapters,
            files=files,
            others=others,
        )

    def beside(self, book: Book, name: str) -> bytes | None:
        """A file beside the book, such as a transcript, or None."""
        ino = book.others.get(name)
        if ino is None:
            return None
        r = self.s.get(f"{self.url}/api/items/{book.id}/file/{ino}", timeout=TIMEOUT)
        return r.content if r.ok else None

    def local(self, book: Book, f: AudioFile) -> Path:
        """The file on this machine: through the shared folder when the
        server's path maps onto one, otherwise downloaded once."""
        for server, here in self.paths.items():
            if f.path.startswith(server + "/"):
                p = Path(here + f.path[len(server):])
                if p.is_file():
                    return p
        dest = self.cache / book.id / f"{f.ino}.{f.ext or 'audio'}"
        if dest.is_file():
            return dest
        dest.parent.mkdir(parents=True, exist_ok=True)
        part = dest.with_suffix(dest.suffix + ".part")
        with self.s.get(f"{self.url}/api/items/{book.id}/file/{f.ino}", stream=True, timeout=TIMEOUT) as r:
            r.raise_for_status()
            with open(part, "wb") as out:
                shutil.copyfileobj(r.raw, out, 1 << 20)
        part.rename(dest)
        return dest

    def forget(self, book: Book) -> None:
        """Drop what was downloaded for a book."""
        shutil.rmtree(self.cache / book.id, ignore_errors=True)


@dataclass
class Song:
    id: str
    artist: str
    title: str
    album: str
    path: str
    seconds: float
    genre: str


class Subsonic:
    def __init__(self, url: str, user: str, password: str, root: Path | None, cache: Path):
        self.url = url
        self.user = user
        self.password = password
        self.root = root
        self.cache = cache
        self.s = requests.Session()

    def _params(self, **more) -> dict:
        salt = secrets.token_hex(6)
        token = hashlib.md5((self.password + salt).encode()).hexdigest()
        return {"u": self.user, "t": token, "s": salt, "v": "1.16.1", "c": "underscore-worker", "f": "json", **more}

    def songs(self) -> list[Song]:
        """Every song, a page at a time. Navidrome lists them all for an
        empty search."""
        out, offset = [], 0
        while True:
            r = self.s.get(
                f"{self.url}/rest/search3",
                params=self._params(query="", songCount=500, songOffset=offset, artistCount=0, albumCount=0),
                timeout=TIMEOUT,
            )
            r.raise_for_status()
            body = r.json()["subsonic-response"]
            if body.get("status") != "ok":
                raise RuntimeError(f"music server: {body.get('error')}")
            page = (body.get("searchResult3") or {}).get("song") or []
            for s in page:
                out.append(
                    Song(
                        id=s["id"],
                        artist=s.get("artist") or "",
                        title=s.get("title") or "",
                        album=s.get("album") or "",
                        path=s.get("path") or "",
                        seconds=float(s.get("duration") or 0),
                        genre=s.get("genre") or "",
                    )
                )
            if len(page) < 500:
                return out
            offset += 500

    def local(self, song: Song) -> Path:
        """The song on this machine: in the shared folder, or downloaded."""
        if self.root is not None and song.path:
            p = self.root / song.path
            if p.is_file():
                return p
        ext = Path(song.path).suffix or ".audio"
        dest = self.cache / f"{song.id}{ext}"
        if not dest.is_file():
            dest.parent.mkdir(parents=True, exist_ok=True)
            r = self.s.get(f"{self.url}/rest/download", params=self._params(id=song.id), timeout=TIMEOUT)
            r.raise_for_status()
            dest.write_bytes(r.content)
        return dest
