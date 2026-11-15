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


def add(notes, text, tags=()):
    """Append a note and return it. Tags keep first-seen order, without duplicates."""
    note = {"id": len(notes) + 1, "text": text, "tags": list(dict.fromkeys(tags))}
    notes.append(note)
    return note


def filter_by_tag(notes, tag):
    """Notes carrying `tag`. Notes saved before tags existed have no `tags` key and never match."""
    return [n for n in notes if tag in n.get("tags", [])]


def render(note):
    tags = note.get("tags", [])
    suffix = "  [" + ", ".join(tags) + "]" if tags else ""
    return f"#{note['id']} {note['text']}{suffix}"


def main(argv=None):
    parser = argparse.ArgumentParser(prog="jot")
    sub = parser.add_subparsers(dest="cmd", required=True)
    p_add = sub.add_parser("add", help="add a note")
    p_add.add_argument("text")
    p_add.add_argument("--tag", action="append", default=[], help="label the note (repeatable)")
    p_list = sub.add_parser("list", help="list notes")
    p_list.add_argument("--tag", help="show only notes with this tag")
    args = parser.parse_args(argv)

    notes = load()
    if args.cmd == "add":
        note = add(notes, args.text, args.tag)
        save(notes)
        print(f"added #{note['id']}")
    else:
        shown = filter_by_tag(notes, args.tag) if args.tag else notes
        for note in shown:
            print(render(note))
    return 0


if __name__ == "__main__":
    sys.exit(main())
