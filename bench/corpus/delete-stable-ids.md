# Design: Delete notes

Status: draft

## Problem

A mistyped note stays in jot forever; people want to remove it.

## Goals and non-goals

Goals: `jot delete ID` removes one note.

Non-goals: undo, bulk delete.

## Technical plan

`jot delete ID` loads `notes.json` ([jot/cli.py](../../jot/cli.py)), removes the note whose `id` equals ID, and saves. If no note has that id it prints `no note #ID` and exits 1. Because `add` assigns `len(notes) + 1`, deleting would let a new note reuse a live id; `add` changes to `max((n["id"] for n in notes), default=0) + 1`, which gives the same ids as today for files that never had a deletion.

## Alternatives considered

**Mark notes as deleted instead of removing them.** Rejected: the file would keep growing with hidden notes.

**Renumber notes after a delete.** Rejected: ids people remember would silently change.

## Risks and mitigations

**Deleting the wrong note.** Mitigation: `delete` prints the removed note's text.

## Implementation map

- `jot/cli.py` — `delete` subcommand, `delete(notes, note_id)` helper, max-based ids in `add`.
- `tests/test_delete.py` — new tests.

## Acceptance criteria

- `jot delete 2` removes note 2 and leaves the others. Check: `tests/test_delete.py`
- After deleting note 2 of 3, `jot add x` gets id 4. Check: `tests/test_delete.py`
- `jot delete 99` prints `no note #99` and exits 1. Check: `tests/test_delete.py`
