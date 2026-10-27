"""The worker's loop: ask the map server for a job, build the map a chapter
at a time starting where the listener is, and send the map after each
chapter, so the phone has music for the next scene long before the book
is done. Between jobs it keeps the music index up to date."""
import json
import sys
import time
import traceback

from . import compose
from .config import Config
from .music import Clap, Index
from .scenes import Scenes
from .servers import Audiobookshelf, MapServer, Subsonic
from .transcribe import Transcriber, Transcript
from .voices import Voices

IDLE = 30
REINDEX = 6 * 3600
NAME = "Made by the server"
TRANSCRIPT_BESIDE = "underscore.transcript.json"


def log(*parts) -> None:
    print("underscore-worker:", *parts, file=sys.stderr, flush=True)


class Worker:
    def __init__(self, cfg: Config):
        self.cfg = cfg
        self.maps = MapServer(cfg.maps_url, cfg.worker_token)
        self.abs = Audiobookshelf(cfg.abs_url, cfg.abs_token, cfg.book_paths, cfg.data / "books-cache")
        self.sub = Subsonic(cfg.subsonic_url, cfg.subsonic_user, cfg.subsonic_password, cfg.music_root,
                            cfg.data / "music-cache")
        self.clap = Clap(cfg.clap_model, cfg.device)
        self.index = Index(cfg.data / "music", self.clap)
        self.whisper = Transcriber(cfg.whisper_model, cfg.device)
        self.scenes = Scenes(cfg.ollama_url, cfg.ollama_model)
        self.indexed = 0.0

    def run(self) -> None:
        log(f"asking {self.cfg.maps_url} for work")
        while True:
            try:
                job = self.maps.claim()
            except Exception as e:
                log(f"map server: {e}")
                time.sleep(IDLE)
                continue
            if job is None:
                if time.time() - self.indexed > REINDEX:
                    self.reindex()
                time.sleep(IDLE)
                continue
            try:
                self.build(job)
            except Exception as e:
                traceback.print_exc()
                try:
                    self.maps.progress(job["id"], state="failed", message=f"Stopped: {e}")
                except Exception:
                    pass

    def reindex(self) -> None:
        try:
            self.index.refresh(self.sub, self.cfg.music_only, log)
            self.indexed = time.time()
        except Exception as e:
            log(f"music index: {e}")

    def build(self, job: dict) -> None:
        jid, map_id = job["id"], job["map"]
        say = lambda message, **more: self.maps.progress(jid, message=message, **more)
        # The server and the phone count chapters from 1.
        numbers = lambda: sorted(k + 1 for k in by_chapter)
        book = self.abs.book(job["book"])
        log(f"job {jid}: {book.title}, from chapter {job['chapter']}")
        chapters = compose.chapters_of(book)
        say(f"Reading {book.title}", total=len(chapters))

        work = self.cfg.data / "jobs" / book.id
        transcript_path = work / "transcript.json"
        given = self.abs.beside(book, TRANSCRIPT_BESIDE) if not transcript_path.is_file() else None
        transcript = (Transcript.adopt(transcript_path, book, given) if given
                      else Transcript(transcript_path, book, self.cfg.whisper_model))
        voices_path = work / "voices.json"
        voices = Voices(self.cfg.ollama_url, self.cfg.ollama_model, book.title,
                        json.loads(voices_path.read_text()) if voices_path.is_file() else None)

        if job.get("kind") == "voices":
            self.voices_only(job, book, chapters, transcript, voices, voices_path)
            return

        self.reindex()
        if not len(self.index.songs):
            raise RuntimeError("the music index is empty: no songs on the music server, or none readable")
        everything = self.sub.songs()

        # Chapters already done for this map, from before a restart.
        state_path = work / f"{map_id}.json"
        by_chapter = {}
        if state_path.is_file():
            ids = {s.id: s for s in everything}
            for k, cues in json.loads(state_path.read_text()).items():
                for c in cues:
                    if "song" in c:
                        c["song"] = ids.get(c["song"]["id"])
                # A song the server no longer has is left out.
                by_chapter[int(k)] = [c for c in cues if c.get("song", True) is not None]

        chooser = compose.Chooser(self.index.songs, self.index.voice)
        wanted = job["chapter"]
        todo = compose.order(len(chapters), wanted, set(by_chapter))
        while todo:
            i = todo.pop(0)
            ch = chapters[i]
            label = f"chapter {ch['number']}" if ch["number"] else f"{compose.clock(ch['start'])}"
            if not transcript.has(i):
                say(f"Transcribing {label}", done=numbers())
                segs = self.whisper.chapter(self.abs, book, ch["start"], ch["end"])
                transcript.put(i, ch["title"], ch["start"], ch["end"], segs)
            ch["segments"] = transcript.segments(i)

            say(f"Reading the scenes in {label}", done=numbers())
            scenes = self.scenes.chapter(ch["number"], ch["title"], ch["start"], ch["end"], ch["segments"])
            moods = [s.get("mood") or s.get("summary") or "calm ambient" for s in scenes]
            sims = self.clap.text(moods) @ self.index.emb.T
            named = lambda artist, title: self.index.find(artist, title, everything)
            by_chapter[i] = compose.cues(ch, scenes, sims, chooser, named)

            state_path.parent.mkdir(parents=True, exist_ok=True)
            state_path.write_text(json.dumps(
                {k: [{**c, "song": {"id": c["song"].id}} if "song" in c else c for c in v] for k, v in by_chapter.items()}
            ))
            complete = len(by_chapter) == len(chapters)
            text = compose.write(map_id, NAME, complete, book, chapters, by_chapter)
            back = self.maps.progress(
                jid, message=f"Finished {label}", done=numbers(),
                total=len(chapters), map=text,
            )
            try:
                self.maps.put_transcript(book.id, transcript.data)
            except Exception as e:
                log(f"transcript: {e}")
            # The listener may have moved; start again from where they are.
            if back.get("chapter") != wanted:
                wanted = back["chapter"]
                todo = compose.order(len(chapters), wanted, set(by_chapter))

        # The map is whole; who says what comes after, so it never holds
        # the music back.
        for i, ch in enumerate(chapters):
            if voices.has(i):
                continue
            label = f"chapter {ch['number']}" if ch["number"] else compose.clock(ch["start"])
            say(f"Map done. Listening for voices in {label}")
            ch["segments"] = transcript.segments(i)
            self.hear_voices(book, i, ch, voices, voices_path)

        self.maps.progress(jid, state="done", message="Done")
        self.abs.forget(book)
        log(f"job {jid}: done")


    def hear_voices(self, book, i: int, ch: dict, voices: Voices, path) -> None:
        """Who says what in chapter `i`, kept and sent. A failure here costs
        only the voices, never the map."""
        if voices.has(i):
            return
        try:
            voices.chapter(i, ch["segments"])
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(json.dumps(voices.data))
            self.maps.put_voices(book.id, voices.data)
        except Exception as e:
            log(f"voices: {e}")

    def voices_only(self, job: dict, book, chapters: list, transcript: Transcript, voices: Voices, path) -> None:
        """A voices job: transcribe what is missing, then work out who says
        what, chapter by chapter from the listener's."""
        jid = job["id"]
        numbers = lambda: sorted(k + 1 for k in voices.by_index)
        wanted = job["chapter"]
        todo = compose.order(len(chapters), wanted, set(voices.by_index))
        while todo:
            i = todo.pop(0)
            ch = chapters[i]
            label = f"chapter {ch['number']}" if ch["number"] else compose.clock(ch["start"])
            if not transcript.has(i):
                self.maps.progress(jid, message=f"Transcribing {label}", done=numbers(), total=len(chapters))
                segs = self.whisper.chapter(self.abs, book, ch["start"], ch["end"])
                transcript.put(i, ch["title"], ch["start"], ch["end"], segs)
            ch["segments"] = transcript.segments(i)
            self.maps.progress(jid, message=f"Listening for voices in {label}", done=numbers(), total=len(chapters))
            self.hear_voices(book, i, ch, voices, path)
            back = self.maps.progress(jid, message=f"Finished {label}", done=numbers(), total=len(chapters))
            if back.get("chapter") != wanted:
                wanted = back["chapter"]
                todo = compose.order(len(chapters), wanted, set(voices.by_index))
        try:
            self.maps.put_transcript(book.id, transcript.data)
        except Exception as e:
            log(f"transcript: {e}")
        self.maps.progress(jid, state="done", message="Done")
        self.abs.forget(book)
        log(f"job {jid}: voices done")


def main() -> None:
    Worker(Config.load()).run()
