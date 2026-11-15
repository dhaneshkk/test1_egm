"""jot: a tiny command-line notebook. Notes live in notes.json in the current directory."""
import argparse
import json
import sys
from pathlib import Path

STORE = Path("notes.json")


def load(path=STORE):
    """Return all notes; an absent file means no notes yet."""
    if not path.exists():
        return []
    return json.loads(path.read_text())


def save(notes, path=STORE):
    path.write_text(json.dumps(notes, indent=2))


def add(notes, text):
    """Append a note and return it. Ids count up from 1."""
    note = {"id": len(notes) + 1, "text": text}
    notes.append(note)
    return note


def render(note):
    return f"#{note['id']} {note['text']}"


def main(argv=None):
    parser = argparse.ArgumentParser(prog="jot")
    sub = parser.add_subparsers(dest="cmd", required=True)
    p_add = sub.add_parser("add", help="add a note")
    p_add.add_argument("text")
    sub.add_parser("list", help="list notes")
    args = parser.parse_args(argv)

    notes = load()
    if args.cmd == "add":
        note = add(notes, args.text)
        save(notes)
        print(f"added #{note['id']}")
    else:
        for note in notes:
            print(render(note))
    return 0


if __name__ == "__main__":
    sys.exit(main())
