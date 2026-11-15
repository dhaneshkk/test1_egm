import tempfile
import unittest
from pathlib import Path

from jot import cli


class JotTest(unittest.TestCase):
    def test_add_assigns_increasing_ids(self):
        notes = []
        cli.add(notes, "first")
        second = cli.add(notes, "second")
        self.assertEqual(second["id"], 2)

    def test_save_and_load_round_trip(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "notes.json"
            notes = []
            cli.add(notes, "hello")
            cli.save(notes, path)
            self.assertEqual(cli.load(path)[0]["text"], "hello")

    def test_missing_store_means_no_notes(self):
        self.assertEqual(cli.load(Path("/nonexistent/notes.json")), [])


if __name__ == "__main__":
    unittest.main()
