"""The parts that need no GPU and no servers: chapter order, song choice,
and the map text. Run with `python -m unittest discover worker/tests`.

`sample-map.toml` is this test's output, checked in. The Rust test
`generated_maps_parse_and_schedule` in crates/mapserver reads it with the
real parser, so a change here that the phone could not read fails there.
Set UNDERSCORE_REGENERATE=1 to write it again after a deliberate change."""
import os
import sys
import unittest
from pathlib import Path
from types import SimpleNamespace

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from underscore_worker import compose  # noqa: E402

SAMPLE = Path(__file__).with_name("sample-map.toml")

SONGS = [
    {"id": "1", "artist": "SideTrax", "title": "Heartbeat", "album": "Pack 02", "seconds": 120.0},
    {"id": "2", "artist": "SideTrax", "title": "News of war", "album": "Pack 20", "seconds": 200.0},
    {"id": "3", "artist": "SideTrax", "title": "Sting", "album": "Pack 01", "seconds": 20.0},
    {"id": "4", "artist": "A Band", "title": "Loud Song", "album": "", "seconds": 240.0},
]

BOOK = SimpleNamespace(title='The "Test" Book', author="An Author", narrator=None, isbn=None, asin="B0TEST",
                       duration=900.0, chapters=[("Opening", 0.0, 300.0), ("One", 300.0, 900.0)])


def segments(start, end):
    return [{"start": t, "end": t + 4, "text": f"Line at {t} seconds here."} for t in np.arange(start, end, 10.0)]


class Compose(unittest.TestCase):
    def test_order_starts_where_the_listener_is(self):
        self.assertEqual(compose.order(5, 3, set()), [2, 3, 4, 0, 1])
        self.assertEqual(compose.order(5, 3, {3}), [2, 4, 0, 1])
        self.assertEqual(compose.order(3, 99, set()), [2, 0, 1])
        self.assertEqual(compose.order(3, 0, set()), [0, 1, 2])

    def test_a_book_without_marks_is_worked_in_stretches(self):
        flat = SimpleNamespace(chapters=[], duration=4000.0)
        parts = compose.chapters_of(flat)
        self.assertEqual([p["number"] for p in parts], [None, None, None])
        self.assertEqual(parts[-1]["end"], 4000.0)

    def test_singing_and_repeats_are_avoided(self):
        voice = np.array([0.0, 0.0, 0.0, 1.0])
        c = compose.Chooser(SONGS, voice)
        sims = np.array([0.30, 0.29, 0.31, 0.40])
        self.assertEqual(c.pick(sims), 0, "the short sting and the song with singing lose")
        self.assertEqual(c.pick(sims), 1, "a song just used gives way to the next best")

    def test_writes_the_sample_map(self):
        chapters = compose.chapters_of(BOOK)
        for ch in chapters:
            ch["segments"] = segments(ch["start"], ch["end"])
        named = lambda artist, title: SimpleNamespace(id="4", artist="A Band", title="Loud Song", album="",
                                                      seconds=60.0) if title == "Loud Song" else None
        c = compose.Chooser(SONGS, np.zeros(len(SONGS)))
        by_chapter = {}
        # Chapter 2 first, as when the listener is there.
        scenes2 = [
            {"start": 300.0, "summary": "a fight", "music": "score", "mood": "action drums", "intensity": 5},
            {"start": 610.0, "summary": "the radio plays", "music": "song", "mood": "", "intensity": 3,
             "song_artist": "A Band", "song_title": "Loud Song"},
            {"start": 800.0, "summary": "a quiet word", "music": "silence", "mood": "", "intensity": 1},
        ]
        by_chapter[1] = compose.cues(chapters[1], scenes2, np.tile([0.1, 0.5, 0.0, 0.0], (3, 1)), c, named)
        scenes1 = [{"start": 0.0, "summary": "opening", "music": "score", "mood": "calm pad", "intensity": 2}]
        by_chapter[0] = compose.cues(chapters[0], scenes1, np.array([[0.5, 0.1, 0.0, 0.0]]), c, named)
        text = compose.write("8f3c0011", "Made by the server", False, BOOK, chapters, by_chapter)

        self.assertIn('id = "8f3c0011"', text)
        self.assertIn("complete = false", text)
        self.assertIn('title = "The \\"Test\\" Book"', text)
        self.assertIn('track = { artist = "A Band", title = "Loud Song" }', text)
        self.assertIn("silence = true", text)
        self.assertLess(text.index("chapter = 1,"), text.index("chapter = 2,"), "chapters in book order")
        if os.environ.get("UNDERSCORE_REGENERATE"):
            SAMPLE.write_text(text)
        self.assertEqual(text, SAMPLE.read_text(), "sample-map.toml is stale; see this file's notes")


if __name__ == "__main__":
    unittest.main()
