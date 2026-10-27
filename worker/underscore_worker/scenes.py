"""Scenes: a local model reads a chapter's transcript and splits it into
stretches, each with the music it wants, described in words.

The model runs in Ollama on the same machine, so the book's text never
leaves the listener's network."""
import json

import requests

# Words in a window: about 25 minutes of narration, well inside the context.
WINDOW_WORDS = 4500

PROMPT = """You are scoring an audiobook with background music, like a film composer.
Below is part of chapter {chapter} ("{title}"), as timed lines. Times are seconds from the start of the chapter.

Split it into scenes, in order. A scene is usually 2 to 8 minutes and changes when the mood, place, or action changes.
For each scene give:
- start: the time of the line where it begins (the first scene starts at {first})
- summary: one short sentence of what happens
- music: "score" for background music, "silence" for moments that need none (a quiet, intimate, or emotionally raw exchange, a pause for effect), or "song" only when the text says a specific real song is playing
- mood: for "score", the music to play, as a short description of sound: instruments, tempo, energy, feeling. For example "tense low strings, slow pulse, ominous". Never vocals.
- intensity: 1 (barely there) to 5 (full action)
- song_artist and song_title: only for "song", the song the text names

Lines:
{lines}"""

SCHEMA = {
    "type": "object",
    "properties": {
        "scenes": {
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "start": {"type": "number"},
                    "summary": {"type": "string"},
                    "music": {"type": "string", "enum": ["score", "silence", "song"]},
                    "mood": {"type": "string"},
                    "intensity": {"type": "integer", "minimum": 1, "maximum": 5},
                    "song_artist": {"type": "string"},
                    "song_title": {"type": "string"},
                },
                "required": ["start", "summary", "music", "mood", "intensity"],
            },
        }
    },
    "required": ["scenes"],
}


def _clock(s: float) -> str:
    m, s = divmod(int(s), 60)
    return f"{m}:{s:02d}"


class Scenes:
    def __init__(self, url: str, model: str):
        self.url = url
        self.model = model

    def chapter(self, number: int | None, title: str, start: float, end: float, segments: list[dict]) -> list[dict]:
        """Scenes for one chapter, with `start` in seconds from the start
        of the book, sorted, the first at the chapter's start."""
        windows, words, current = [], 0, []
        for s in segments:
            current.append(s)
            words += len(s["text"].split())
            if words >= WINDOW_WORDS:
                windows.append(current)
                current, words = [], 0
        if current:
            windows.append(current)

        scenes = []
        for w in windows:
            first = round(w[0]["start"] - start) if scenes else 0
            lines = "\n".join(f"[{_clock(s['start'] - start)} = {round(s['start'] - start)}] {s['text']}" for s in w)
            prompt = PROMPT.format(chapter=number or "", title=title or "untitled", first=first, lines=lines)
            for scene in self._ask(prompt):
                at = start + float(scene.get("start") or 0)
                if not start <= at < end:
                    continue
                scene["start"] = at
                scene["intensity"] = min(5, max(1, int(scene.get("intensity") or 2)))
                scenes.append(scene)
        if not scenes:
            scenes = [{"start": start, "summary": title, "music": "score", "mood": "soft ambient pad, calm", "intensity": 2}]
        scenes.sort(key=lambda s: s["start"])
        scenes[0]["start"] = start
        # Each scene starts where a line starts, so music never comes in mid-word.
        starts = [s["start"] for s in segments]
        for s in scenes[1:]:
            if starts:
                s["start"] = min(starts, key=lambda t: abs(t - s["start"]))
        # Two scenes at one time are one.
        out = []
        for s in scenes:
            if out and s["start"] - out[-1]["start"] < 20:
                continue
            out.append(s)
        return out

    def _ask(self, prompt: str) -> list[dict]:
        r = requests.post(
            f"{self.url}/api/chat",
            json={
                "model": self.model,
                "messages": [{"role": "user", "content": prompt}],
                "format": SCHEMA,
                "stream": False,
                "think": False,
                "options": {"temperature": 0.3, "num_ctx": 16384},
            },
            timeout=900,
        )
        r.raise_for_status()
        text = r.json()["message"]["content"]
        try:
            return json.loads(text).get("scenes") or []
        except json.JSONDecodeError:
            return []
