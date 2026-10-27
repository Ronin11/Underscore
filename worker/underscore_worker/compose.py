"""Scenes into cues, and cues into a map.

A scene's mood is matched to the index by CLAP similarity, with a few
nudges: away from songs too short to settle under a scene, from songs
just used, from songs used a lot, and from songs with singing. A scene
longer than its song gets the next best song after it, or the song
again, looped, when little is left. A song the book names plays where
the book names it, when the library has it.

The map is the format in docs/music-map.md: cues anchored to chapters,
each with its own `until`, so chapters can be filled in any order and a
chapter not reached yet stays quiet."""
import json
import math

import numpy as np

# A book with no chapter marks is worked through in stretches this long.
STRETCH = 30 * 60
# Intensity 1 to 5 as a level offset, in decibels.
LEVEL = {1: -6, 2: -4, 3: -2, 4: 0, 5: 0}
SHORT = 45.0
RECENT = 8


def chapters_of(book) -> list[dict]:
    """The book's chapters, numbered from 1 as the phone counts them. With
    no marks, stretches of the whole book, which the map places by time."""
    if book.chapters:
        return [{"number": i + 1, "title": t, "start": s, "end": e} for i, (t, s, e) in enumerate(book.chapters)]
    out, t = [], 0.0
    while t < book.duration:
        out.append({"number": None, "title": "", "start": t, "end": min(book.duration, t + STRETCH)})
        t += STRETCH
    return out


def order(n: int, wanted: int, done: set) -> list[int]:
    """Chapter indices still to do: from the listener's on, then the ones before."""
    start = min(max(wanted - 1, 0), max(n - 1, 0))
    return [i for i in list(range(start, n)) + list(range(0, start)) if i not in done]


def clock(x: float) -> str:
    """A time as the map writes it, rounded down to a tenth, so a cue's end
    never passes the start of the next chapter's first cue."""
    x = math.floor(max(0.0, x) * 10) / 10
    h, r = divmod(x, 3600)
    m, s = divmod(r, 60)
    return f"{int(h)}:{int(m):02d}:{s:04.1f}" if h else f"{int(m)}:{s:04.1f}"


def anchor(segments: list[dict], at: float) -> str | None:
    """The first words of the line at `at`."""
    for s in segments:
        if s["start"] >= at - 0.5:
            words = s["text"].split()
            return " ".join(words[:10]) if words else None
    return None


class Chooser:
    """Picks songs for scenes across a whole book, remembering what it used."""

    def __init__(self, songs: list[dict], voice: np.ndarray):
        self.songs = songs
        self.voice = voice
        self.used: dict[int, int] = {}
        self.recent: list[int] = []

    def pick(self, sims: np.ndarray) -> int:
        s = sims.copy()
        s -= np.array([0.2 if t["seconds"] < SHORT else 0.0 for t in self.songs])
        s -= 0.5 * self.voice
        for k in self.recent[-RECENT:]:
            s[k] -= 0.15
        for k, n in self.used.items():
            s[k] -= 0.03 * n
        k = int(np.argmax(s))
        self.used[k] = self.used.get(k, 0) + 1
        self.recent.append(k)
        return k


def cues(chapter: dict, scenes: list[dict], sims: np.ndarray, chooser: Chooser, named) -> list[dict]:
    """Cues for one chapter. `chapter` has `number` (or None), `start`,
    `end`, and `segments`; `sims[i]` scores every song against scene i's
    mood; `named(artist, title)` finds a song the book names, or None."""
    out = []
    ends = [s["start"] for s in scenes[1:]] + [chapter["end"]]
    for i, (scene, end) in enumerate(zip(scenes, ends)):
        t, first = scene["start"], True
        why = scene.get("summary") or ""
        if scene.get("music") == "silence":
            out.append({"start": t, "end": end, "silence": True, "why": why,
                        "anchor": anchor(chapter["segments"], t)})
            continue
        if scene.get("music") == "song":
            song = named(scene.get("song_artist") or "", scene.get("song_title") or "")
            if song is not None:
                stop = min(end, t + song.seconds)
                out.append({"start": t, "end": stop, "song": song, "why": f"the book plays {song.title}",
                            "anchor": anchor(chapter["segments"], t)})
                t, first = stop, False
        while end - t > 1:
            k = chooser.pick(sims[i])
            song = chooser.songs[k]
            take = min(end - t, song["seconds"])
            if end - t - take < 40:
                take = end - t
            last = out[-1] if out else None
            if last is not None and not first and last.get("track") is song and last["end"] == t:
                # The same song again: let the cue it is in repeat instead.
                last["end"] = t + take
                last["loop"] = True
                t += take
                continue
            out.append({
                "start": t, "end": t + take, "track": song, "why": scene.get("mood") or why,
                "intensity": scene.get("intensity", 2),
                "anchor": anchor(chapter["segments"], t) if first else None,
                "loop": take > song["seconds"] + 0.5,
            })
            t += take
            first = False
    return out


def _at(chapter: dict, t: float) -> str:
    off = clock(t - chapter["start"])
    if chapter["number"] is None:
        return json.dumps(off)
    return f'{{ chapter = {chapter["number"]}, offset = "{off}" }}'


def _q(s: str) -> str:
    return json.dumps(s, ensure_ascii=False)


def _track(artist: str, title: str, album: str) -> str:
    album = f", album = {_q(album)}" if album else ""
    return f"track = {{ artist = {_q(artist)}, title = {_q(title)}{album} }}"


def write(map_id: str, name: str, complete: bool, book, chapters: list[dict], by_chapter: dict) -> str:
    """The whole map so far. `by_chapter` holds each finished chapter's cues
    by its index into `chapters`."""
    lines = ["version = 1", f"id = {_q(map_id)}", f"name = {_q(name)}"]
    if not complete:
        lines.append("complete = false")
    lines += ["", "[book]", f"title = {_q(book.title)}"]
    for key, value in (("author", book.author), ("narrator", book.narrator), ("isbn", book.isbn), ("asin", book.asin)):
        if value:
            lines.append(f"{key} = {_q(value)}")
    lines += [f'duration = "{clock(book.duration)}"', "",
              "[defaults]", 'fade_in = "6"', 'fade_out = "6"', 'crossfade = "5"', "level_db = -6", ""]
    for index in sorted(by_chapter):
        ch = chapters[index]
        for c in by_chapter[index]:
            lines += ["[[cue]]", f"at = {_at(ch, c['start'])}", f"until = {_at(ch, c['end'])}"]
            if c.get("silence"):
                lines.append("silence = true")
            elif "song" in c:
                s = c["song"]
                lines += [_track(s.artist, s.title, s.album), "level_db = 0", 'fade_in = "2"']
            else:
                s = c["track"]
                lines.append(_track(s["artist"], s["title"], s["album"]))
                if c.get("loop"):
                    lines.append('region = { start = "0:00", repeat = true, crossfade = "3" }')
                level = LEVEL.get(c.get("intensity", 2), -4)
                if level != -6:
                    lines.append(f"level_db = {level}")
            if c.get("why"):
                lines.append(f"why = {_q(c['why'])}")
            if c.get("anchor"):
                lines.append(f"anchor = {_q(c['anchor'])}")
            lines.append("")
    return "\n".join(lines)
