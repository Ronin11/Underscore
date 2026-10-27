"""Taking a transcript made before, without transcribing again."""
import json
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from underscore_worker.transcribe import Transcript  # noqa: E402

BOOK = SimpleNamespace(title="B", duration=300.0, chapters=[("One", 0.0, 100.0), ("Two", 100.0, 200.0), ("Three", 200.0, 300.0)])


class Adopt(unittest.TestCase):
    def path(self):
        return Path(tempfile.mkdtemp()) / "t.json"

    def test_a_partial_server_transcript_leaves_the_rest_to_do(self):
        given = {"chapters": [
            {"index": 0, "segments": [{"start": 1.0, "end": 2.0, "text": "a"}]},
            {"index": 2, "segments": [{"start": 201.0, "end": 202.0, "text": "c"}]},
        ]}
        t = Transcript.adopt(self.path(), BOOK, json.dumps(given).encode())
        self.assertTrue(t.has(0))
        self.assertFalse(t.has(1), "a chapter the server never had is still to transcribe")
        self.assertTrue(t.has(2))

    def test_flat_segments_fill_only_the_chapters_they_reach(self):
        given = {"segments": [{"start": 150.0, "end": 151.0, "text": "b"}]}
        t = Transcript.adopt(self.path(), BOOK, json.dumps(given).encode())
        self.assertEqual([t.has(i) for i in range(3)], [False, True, False])

    def test_a_book_without_marks_is_taken_in_stretches(self):
        flat = SimpleNamespace(title="F", duration=4000.0, chapters=[])
        given = {"segments": [{"start": 10.0, "end": 11.0, "text": "x"}, {"start": 3700.0, "end": 3701.0, "text": "y"}]}
        t = Transcript.adopt(self.path(), flat, json.dumps(given).encode())
        self.assertEqual([t.has(i) for i in range(3)], [True, False, True])


if __name__ == "__main__":
    unittest.main()
