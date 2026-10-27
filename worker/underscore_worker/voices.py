"""Who says what: the transcript's lines split into spans, each with a
speaker, so subtitles can show who is talking.

A local model reads a chapter's lines, a few dozen at a time, and splits
every line into spans of the exact text, each with who says it. A line it
leaves out is the narrator's. A span list that does not add up
to its line is not trusted: the line goes whole to whoever said most of
it. So a bad answer can mislabel a line, never garble one.

The file, as the phone reads it:

    {"version": 1,
     "cast": [{"id": "grace", "name": "Ryland Grace"}, ...],
     "chapters": [{"index": 2, "lines": [
         {"start": 17.4, "end": 21.0, "spans": [{"who": "narrator", "text": "..."}]}]}]}

Times are seconds from the start of the book. "narrator" is reserved.
"""
import json
import re

import requests

NARRATOR = "narrator"
# Lines per question. Small enough that the model reads each one; it is
# slow (with reasoning on, about two and a half seconds a line on a 3090)
# but far better than fast: without reasoning it gives dialogue to the
# wrong speaker, or finds none at all.
WINDOW = 30
# Lines before each window, shown for context and not labelled.
CONTEXT = 6

PROMPT = """You are preparing subtitles for the audiobook "{title}". One narrator reads everything, including every character's dialogue. The story may be told in the first person; when the narrator character speaks aloud, give those words that character's id, not "narrator". The transcript has no quotation marks, so work out from context which words are spoken aloud and by whom: speech tags like "I say" or "he says", who is answering whom, and how each character talks. Thoughts and jokes the narrator makes to the listener are narration, not speech.

For EVERY numbered line, split its text into spans in order and say who says each span:
- "narrator" for narration, description, thoughts, and speech tags such as "I say" or "she says".
- a character id (short, lowercase) for words that character speaks aloud.
Copy span text exactly from the line. The spans of a line must join back into the whole line.

Example: the line "Yes, I say, we covered this." gives spans [{{"who":"anna","text":"Yes,"}},{{"who":"narrator","text":"I say,"}},{{"who":"anna","text":"we covered this."}}].

Characters known so far (reuse these ids; add anyone new to "cast" with an id and full name):
{cast}

Just before these lines (for context only, do not label):
{before}

Lines:
{lines}"""

SCHEMA = {
    "type": "object",
    "properties": {
        "cast": {
            "type": "array",
            "items": {
                "type": "object",
                "properties": {"id": {"type": "string"}, "name": {"type": "string"}},
                "required": ["id", "name"],
            },
        },
        "lines": {
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "n": {"type": "integer"},
                    "spans": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {"who": {"type": "string"}, "text": {"type": "string"}},
                            "required": ["who", "text"],
                        },
                    },
                },
                "required": ["n", "spans"],
            },
        },
    },
    "required": ["cast", "lines"],
}


def _norm(s: str) -> str:
    return "".join(c for c in s.lower() if c.isalnum())


def _id(s: str) -> str:
    return re.sub(r"[^a-z0-9]+", "-", s.lower()).strip("-") or NARRATOR


def settle(text: str, spans: list[dict], known: set) -> list[dict]:
    """The spans for one line, checked: they must cover the line exactly
    (ignoring spacing and punctuation) and name known speakers. Otherwise
    the whole line goes to whoever has the most words in the answer."""
    spans = [{"who": _id(s.get("who", "")), "text": s.get("text", "")} for s in spans if s.get("text", "").strip()]
    for s in spans:
        if s["who"] not in known:
            s["who"] = NARRATOR
    if spans and _norm("".join(s["text"] for s in spans)) == _norm(text):
        # Merge neighbours with the same speaker.
        out = []
        for s in spans:
            if out and out[-1]["who"] == s["who"]:
                out[-1]["text"] = (out[-1]["text"].rstrip() + " " + s["text"].lstrip()).strip()
            else:
                out.append({"who": s["who"], "text": s["text"].strip()})
        return out
    words = {}
    for s in spans:
        words[s["who"]] = words.get(s["who"], 0) + len(s["text"].split())
    who = max(words, key=words.get) if words else NARRATOR
    return [{"who": who, "text": text}]


class Voices:
    """One book's voices file, filled chapter by chapter."""

    def __init__(self, url: str, model: str, title: str, data: dict | None = None):
        self.url = url
        self.model = model
        self.title = title
        self.data = data or {"version": 1, "cast": [], "chapters": []}
        self.by_index = {c["index"]: c for c in self.data["chapters"]}

    def has(self, index: int) -> bool:
        return index in self.by_index

    def known(self) -> set:
        return {NARRATOR} | {c["id"] for c in self.data["cast"]}

    def chapter(self, index: int, segments: list[dict]) -> None:
        """Work out who says what in one chapter's segments."""
        lines = [{"start": g["start"], "end": g["end"], "spans": [{"who": NARRATOR, "text": g["text"]}]} for g in segments]
        for w in range(0, len(segments), WINDOW):
            part = segments[w:w + WINDOW]
            answer = self._ask(part, segments[max(0, w - CONTEXT):w])
            for c in answer.get("cast") or []:
                cid = _id(c.get("id") or c.get("name") or "")
                if cid != NARRATOR and cid not in self.known():
                    self.data["cast"].append({"id": cid, "name": (c.get("name") or cid).strip()})
            known = self.known()
            for item in answer.get("lines") or []:
                n = item.get("n")
                if not isinstance(n, int) or not 1 <= n <= len(part):
                    continue
                g = part[n - 1]
                lines[w + n - 1]["spans"] = settle(g["text"], item.get("spans") or [], known)
        self.by_index[index] = {"index": index, "lines": lines}
        self.data["chapters"] = [self.by_index[i] for i in sorted(self.by_index)]

    def _ask(self, part: list[dict], before: list[dict]) -> dict:
        cast = "\n".join(f"- {c['id']}: {c['name']}" for c in self.data["cast"]) or "(none yet)"
        lines = "\n".join(f"{i}: {g['text']}" for i, g in enumerate(part, 1))
        context = " ".join(g["text"] for g in before) or "(the start of the chapter)"
        try:
            r = requests.post(
                f"{self.url}/api/chat",
                json={
                    "model": self.model,
                    "messages": [{"role": "user", "content": PROMPT.format(title=self.title, cast=cast, before=context, lines=lines)}],
                    "format": SCHEMA,
                    "stream": False,
                    "think": True,
                    "options": {"temperature": 0.1, "num_ctx": 24576},
                },
                timeout=1800,
            )
            r.raise_for_status()
            return json.loads(r.json()["message"]["content"])
        except (requests.RequestException, ValueError, KeyError):
            # This stretch stays the narrator's; the rest goes on.
            return {}
