import unittest

from underscore_worker.servers import clean_title


class CleanTitle(unittest.TestCase):
    def test_store_tags_and_repeated_subtitles_go(self):
        self.assertEqual(clean_title("Zero to One (Unabridged)", "Notes on Startups"), "Zero to One")
        self.assertEqual(
            clean_title(
                "Willpower: Rediscovering the Greatest Human Strength (Unabridged)",
                "Rediscovering the Greatest Human Strength",
            ),
            "Willpower",
        )
        self.assertEqual(clean_title("Upgrade [UNABRIDGED] ", "A Novel"), "Upgrade")

    def test_the_rest_stays(self):
        self.assertEqual(clean_title("Dune", "Book One"), "Dune")
        self.assertEqual(clean_title("Red Rising", "Rising"), "Red Rising")
        self.assertEqual(clean_title("Dune (Abridged)", None), "Dune (Abridged)")
        self.assertEqual(clean_title("(Unabridged)", None), "(Unabridged)")


if __name__ == "__main__":
    unittest.main()
