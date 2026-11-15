"""Build the corpus's defective twins and its labels, so that no "not ready" label rests
on anyone's judgment of a whole doc.

Each twin is a ready doc with one scripted edit (an exact text replacement that must
apply exactly once) that removes or contradicts something an implementer needs. The
edit and the defect it creates are recorded with the label. The ready docs are labelled
by review against the rubric below; ask someone who did not write them to re-check those.

Splits: the 10 original docs tuned the false-alarm rules, so they are `dev`; the 20 docs
added with this script were never used for tuning, so they are the held-out `test` split.

Rubric for "ready": a competent engineer can implement it in one pass without a design
decision the doc leaves open; how old data behaves, error cases, formats and ids are
stated where they matter; every acceptance criterion names a check.

usage: python3 bench/make_twins.py   (rewrites the twins and corpus/labels.json)
"""
import json
from pathlib import Path

CORPUS = Path(__file__).resolve().parent / "corpus"

# twin: (base doc, defect class, what the twin lacks, [(old, new), ...])
TWINS = {
    "edit-no-error": ("edit-complete", "missing error case", "what an unknown id does",
        [(" An unknown ID prints `no note #ID` to stderr, changes nothing and exits 1.", ""),
         ("\n- `jot edit 9 x` with no note 9 prints `no note #9` to stderr, exits 1, and leaves `notes.json` unchanged. Check: `tests/test_edit.py`", "")]),
    "pin-no-old-data": ("pin-complete", "missing old-data rule", "how notes saved before pins behave",
        [(" Notes without a `pinned` key, including every note saved before this change, count as unpinned; jot never rewrites them.", ""),
         ("\n- A `notes.json` with no `pinned` keys lists in id order. Check: `tests/test_pin.py`", "")]),
    "done-contradiction": ("done-complete", "contradictory criterion", "agreement between the plan and a criterion",
        [("- After `jot done 1`, `jot list` does not print note 1.", "- After `jot done 1`, `jot list` still prints note 1.")]),
    "due-vague-format": ("due-complete", "undefined format", "the date format and what happens to a bad date",
        [("Dates are ISO `YYYY-MM-DD`, parsed with `datetime.date.fromisoformat`; anything else is rejected by argparse with exit 2. ", "Dates are written in the usual way. "),
         ("\n- `jot add x --due tomorrow` exits 2 and adds nothing. Check: `tests/test_due.py`", "")]),
    "count-undefined-term": ("count-complete", "undefined term", "which notes count",
        [("prints the number of notes.", "prints the number of active notes."),
         ("prints `len(notes)` as a bare integer", "prints the number of active notes as a bare integer"),
         ("- With three notes, `jot count` prints `3`.", "- With three active notes, `jot count` prints `3`.")]),
    "csv-no-quoting": ("csv-complete", "missing format rule", "how text with commas or quotes is written",
        [(" with Python's `csv.writer` (RFC 4180 quoting, so text with commas, quotes or newlines survives):", ":"),
         ("- Exporting notes `a` and `b, \"c\"` writes a file that `csv.reader` reads back as `[['id','text'],['1','a'],['2','b, \"c\"']]`. Check: `tests/test_csv.py`\n", "")]),
    "undo-contradiction": ("undo-complete", "contradictory criterion", "agreement between the plan and a criterion",
        [("- With notes 1-3, `jot undo` removes note 3 and prints `removed #3`.", "- With notes 1-3, `jot undo` removes note 1 and prints `removed #1`.")]),
    "show-no-error": ("show-complete", "missing error case", "what an unknown id does",
        [(" An unknown ID prints `no note #ID` to stderr and exits 1.", ""),
         ("\n- `jot show 9` with no note 9 prints `no note #9` to stderr and exits 1. Check: `tests/test_show.py`", "")]),
    "import-vague-format": ("import-complete", "undefined format", "the input file's format",
        [("FILE is UTF-8 text, one note per line. `jot import FILE` reads it, strips each line's trailing newline and surrounding spaces, skips empty lines, and adds each remaining line",
          "`jot import FILE` reads the notes in FILE and adds each one"),
         ("- Importing a file with lines `a`, empty, `  b  ` adds notes `a` and `b` with the next two ids.", "- Importing a file adds its notes with the next ids.")]),
    "archive-reused-ids": ("archive-complete", "id reuse", "an id rule that keeps archived ids unique",
        [(" Archived notes keep their id, and `jot add` never reuses one: a new note's id is one more than the highest id in `notes.json` and `archive.json` together, replacing `len(notes) + 1` in `add` ({CLI}).".replace("{CLI}", "[jot/cli.py](../../jot/cli.py)"), " Archived notes keep their id."),
         (", id rule in `add`", ""),
         ("\n- With notes 1-3 and note 3 archived, `jot add x` gets id 4. Check: `tests/test_archive.py`", "")]),
}

ORIGINAL = {  # the first 10 docs (dev split): labels as before
    "tags-complete": "ready", "tags-no-old-data": "not-ready", "delete-reused-ids": "not-ready", "delete-stable-ids": "ready",
    "search-complete": "ready", "export-vague": "not-ready", "priority-no-old-data": "not-ready", "priority-complete": "ready",
    "purge-reused-ids": "not-ready", "purge-stable-ids": "ready",
}


def main():
    labels = {slug: {"label": lab, "split": "dev"} for slug, lab in ORIGINAL.items()}
    for twin, (base, kind, lacks, edits) in TWINS.items():
        text = (CORPUS / f"{base}.md").read_text()
        for old, new in edits:
            n = text.count(old)
            if n != 1:
                raise SystemExit(f"{twin}: an edit applies {n} times, not once: {old[:60]!r}")
            text = text.replace(old, new)
        (CORPUS / f"{twin}.md").write_text(text)
        labels[base] = {"label": "ready", "split": "test", "pair": twin}
        labels[twin] = {"label": "not-ready", "split": "test", "pair": base, "defect": kind, "lacks": lacks,
                        "edits": len(edits)}
    (CORPUS / "labels.json").write_text(json.dumps(labels, indent=1) + "\n")
    n = {s: sum(v["split"] == s for v in labels.values()) for s in ("dev", "test")}
    print(f"{len(labels)} docs: {n['dev']} dev, {n['test']} test ({len(TWINS)} pairs)")


if __name__ == "__main__":
    main()
