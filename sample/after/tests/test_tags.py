import json
import tempfile
import unittest
from pathlib import Path

from jot import cli


class TagTest(unittest.TestCase):
    def test_add_stores_tags_without_duplicates(self):
        notes = []
        note = cli.add(notes, "standup", ["work", "daily", "work"])
        self.assertEqual(note["tags"], ["work", "daily"])

    def test_filter_returns_only_tagged_notes(self):
        notes = []
        cli.add(notes, "standup", ["work"])
        cli.add(notes, "groceries", ["home"])
        self.assertEqual([n["text"] for n in cli.filter_by_tag(notes, "work")], ["standup"])

    def test_notes_saved_before_tags_still_load_and_never_match(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "notes.json"
            path.write_text(json.dumps([{"id": 1, "text": "old note"}]))
            notes = cli.load(path)
            self.assertEqual(cli.filter_by_tag(notes, "work"), [])
            self.assertEqual(cli.render(notes[0]), "#1 old note")


if __name__ == "__main__":
    unittest.main()
