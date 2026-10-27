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
from difflib import SequenceMatcher

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


OTHER = "other"
OTHER_NAME = "Other voices"
# Lines of each speaker shown when settling the cast.
SAMPLES = 3

CAST_PROMPT = """These are the speakers found, chapter by chapter, in the audiobook "{title}", each with an id, the name it was given, how many words it says, and a few of its lines. The same person often appears under more than one id: a first name in one chapter and a surname in another, a title, or a misspelling. A narrator who is also a character may appear as that character.

Group the ids that are the same person. Give each person one id, reusing the id of theirs that says the most, and their best full name as the book would print it. Speakers who are no one in particular, such as "someone", "voice", or "a woman", go together under the id "other" with the name "Other voices". List every id exactly once, in one group.

Speakers:
{rows}"""

CAST_SCHEMA = {
    "type": "object",
    "properties": {
        "people": {
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "id": {"type": "string"},
                    "name": {"type": "string"},
                    "ids": {"type": "array", "items": {"type": "string"}},
                },
                "required": ["id", "name", "ids"],
            },
        }
    },
    "required": ["people"],
}


# Speakers that are no one in particular, which may join "other".
VAGUE = {"someone", "voice", "voices", "woman", "man", "person", "people", "somebody", "crowd", "other", "unknown"}


def _words(name: str) -> set:
    return {w for w in re.split(r"[^a-z]+", name.lower()) if len(w) >= 3 and w not in {"the", "doctor", "mister"}}


def same_person(a: str, b: str) -> bool:
    """Whether two speaker names can be one person: they share a word,
    allowing a slip of a letter or two ("Strat" and "Eva Stratt"). The
    model's grouping is only taken where this holds; it once folded two
    other characters into the narrator because their sample lines had been
    misread."""
    for x in _words(a):
        for y in _words(b):
            if x == y or (min(len(x), len(y)) >= 4 and SequenceMatcher(None, x, y).ratio() >= 0.8):
                return True
    return False


def by_name(ids: list, names: dict, words: dict) -> dict:
    """Speakers whose names can be one person, each to the one of them who
    says the most. Vague speakers are left to the model."""
    order = sorted(ids, key=lambda i: -words.get(i, 0))
    out = {}
    for n, a in enumerate(order):
        if a in VAGUE:
            continue
        for b in order[:n]:
            if b in VAGUE or b in out:
                continue
            if same_person(names.get(a, a), names.get(b, b)):
                out[a] = b
                break
    return out


def settle_people(people: list[dict], known: set, names: dict | None = None) -> dict:
    """The model's grouping as old id to new, checked: only known ids move,
    each at most once, a group's id is one of its own or "other", only a
    vague speaker may join "other", and two ids merge only when their names
    can be one person. An id left out keeps itself."""
    names = names or {}
    name = lambda i: names.get(i, i)
    moves = {}
    for p in people:
        target = _id(p.get("id") or "")
        members = [_id(i) for i in p.get("ids") or [] if _id(i) in known]
        if not members:
            continue
        if target not in members and target != OTHER:
            # A new id would lose the name the lines were known by; use the group's own.
            target = members[0]
        for m in members:
            if m in moves:
                continue
            if target == OTHER:
                ok = m in VAGUE or _words(name(m)) <= VAGUE or not _words(name(m))
            else:
                ok = m == target or same_person(name(m), name(target)) or same_person(m, target)
            moves[m] = target if ok else m
    return moves


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
        # A stretch the model cannot answer stays the narrator's; the rest goes on.
        return self._chat(PROMPT.format(title=self.title, cast=cast, before=context, lines=lines), SCHEMA)

    def _chat(self, prompt: str, schema: dict) -> dict:
        try:
            r = requests.post(
                f"{self.url}/api/chat",
                json={
                    "model": self.model,
                    "messages": [{"role": "user", "content": prompt}],
                    "format": schema,
                    "stream": False,
                    "think": True,
                    "options": {"temperature": 0.1, "num_ctx": 24576},
                },
                timeout=1800,
            )
            r.raise_for_status()
            return json.loads(r.json()["message"]["content"])
        except (requests.RequestException, ValueError, KeyError):
            return {}

    def words(self) -> dict:
        """Words each speaker says, across the book."""
        out = {}
        for ch in self.data["chapters"]:
            for line in ch["lines"]:
                for sp in line["spans"]:
                    out[sp["who"]] = out.get(sp["who"], 0) + len(sp["text"].split())
        return out

    def settle_cast(self) -> dict:
        """Once the book is done: one id and one name per person. The model
        sees every speaker with how much they say and a few of their lines,
        and groups the ids that are the same person, such as a first name in
        one chapter and a surname in another. Speakers who are no one in
        particular join "other". Then every line is relabelled, and the cast
        is ordered by how much each says, so the main voices get the first
        colors. Returns the ids that changed, old to new."""
        words = self.words()
        samples = {}
        for ch in self.data["chapters"]:
            for line in ch["lines"]:
                for sp in line["spans"]:
                    got = samples.setdefault(sp["who"], [])
                    if len(got) < SAMPLES and len(sp["text"]) > 12:
                        got.append(sp["text"][:90])
        names = {c["id"]: c["name"] for c in self.data["cast"]}
        speakers = [cid for cid in names if words.get(cid)]
        if len(speakers) < 2:
            return {}
        rows = "\n".join(
            f"- {cid} ({names[cid]}), {words[cid]} words: " + " / ".join(f'"{t}"' for t in samples.get(cid, []))
            for cid in sorted(speakers, key=lambda c: -words[c])
        )
        answer = self._chat(CAST_PROMPT.format(title=self.title, rows=rows), CAST_SCHEMA)
        moves = settle_people(answer.get("people") or [], set(speakers), names)
        # Names that plainly match merge whatever the model said; its answers
        # vary from run to run, and "Strat" and "Eva Stratt" should not.
        for a, b in by_name(speakers, names, words).items():
            if moves.get(a, a) == a:
                moves[a] = moves.get(b, b)
        if not any(old != new for old, new in moves.items()) and not answer.get("people"):
            return {}
        people = {p["id"]: p["name"] for p in answer.get("people") or [] if p.get("id") in moves.values()}
        for ch in self.data["chapters"]:
            for line in ch["lines"]:
                for sp in line["spans"]:
                    sp["who"] = moves.get(sp["who"], sp["who"])
                # Neighbours that are now the same speaker read as one span.
                merged = []
                for sp in line["spans"]:
                    if merged and merged[-1]["who"] == sp["who"]:
                        merged[-1]["text"] = (merged[-1]["text"] + " " + sp["text"]).strip()
                    else:
                        merged.append(sp)
                line["spans"] = merged
        after = self.words()
        cast = {}
        for cid, name in names.items():
            target = moves.get(cid, cid)
            if target == NARRATOR or not after.get(target):
                continue
            cast[target] = people.get(target) or cast.get(target) or (OTHER_NAME if target == OTHER else name)
        self.data["cast"] = [{"id": cid, "name": cast[cid]} for cid in sorted(cast, key=lambda c: -after[c])]
        self.data["cast_settled"] = True
        return {old: new for old, new in moves.items() if old != new}
