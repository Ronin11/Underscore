"""Checking the model's answer about who says what. No model needed."""
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from underscore_worker.voices import NARRATOR, Voices, settle  # noqa: E402

KNOWN = {NARRATOR, "grace", "rocky"}


class Settle(unittest.TestCase):
    def test_spans_that_cover_the_line_are_kept_and_merged(self):
        line = "I'm not going back, I said. He stares at me."
        spans = [{"who": "grace", "text": "I'm not going back,"}, {"who": "narrator", "text": "I said."},
                 {"who": "narrator", "text": "He stares at me."}]
        self.assertEqual(settle(line, spans, KNOWN), [
            {"who": "grace", "text": "I'm not going back,"},
            {"who": "narrator", "text": "I said. He stares at me."},
        ])

    def test_spacing_and_punctuation_differences_are_forgiven(self):
        line = "Fist my bump, he says."
        spans = [{"who": "rocky", "text": "Fist my bump"}, {"who": "narrator", "text": "he says"}]
        self.assertEqual([s["who"] for s in settle(line, spans, KNOWN)], ["rocky", "narrator"])

    def test_spans_that_change_the_words_give_the_line_to_the_main_speaker(self):
        line = "Amaze, amaze, amaze, Rocky says."
        spans = [{"who": "rocky", "text": "Amaze amaze amaze amaze"}, {"who": "narrator", "text": "Rocky says."}]
        self.assertEqual(settle(line, spans, KNOWN), [{"who": "rocky", "text": line}])

    def test_unknown_speakers_become_the_narrator(self):
        line = "Hello."
        self.assertEqual(settle(line, [{"who": "Stranger", "text": "Hello."}], KNOWN), [{"who": NARRATOR, "text": "Hello."}])

    def test_nothing_said_is_the_narrator(self):
        self.assertEqual(settle("Quiet.", [], KNOWN), [{"who": NARRATOR, "text": "Quiet."}])


class Chapter(unittest.TestCase):
    def test_lines_the_model_skips_stay_with_the_narrator_and_new_cast_is_kept(self):
        v = Voices("http://unused", "m", "Book")
        v._ask = lambda part, before: {
            "cast": [{"id": "Rocky", "name": "Rocky"}],
            "lines": [{"n": 2, "spans": [{"who": "rocky", "text": "Grace question?"}]}, {"n": 99, "spans": []}],
        }
        segs = [{"start": 1.0, "end": 2.0, "text": "The hull crackles."}, {"start": 2.0, "end": 3.0, "text": "Grace question?"}]
        v.chapter(5, segs)
        self.assertEqual(v.data["cast"], [{"id": "rocky", "name": "Rocky"}])
        lines = v.data["chapters"][0]["lines"]
        self.assertEqual(lines[0]["spans"], [{"who": NARRATOR, "text": "The hull crackles."}])
        self.assertEqual(lines[1]["spans"], [{"who": "rocky", "text": "Grace question?"}])
        self.assertTrue(v.has(5))


if __name__ == "__main__":
    unittest.main()
