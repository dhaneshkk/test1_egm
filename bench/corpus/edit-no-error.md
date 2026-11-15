# Design: Edit a note

Status: draft

## Problem

A typo in a note can only be fixed by editing `notes.json` by hand.

## Goals and non-goals

Goals: `jot edit ID TEXT` replaces the text of note ID.

Non-goals: editing several notes at once, edit history.

## Technical plan

`jot edit ID TEXT` loads notes ([jot/cli.py](../../jot/cli.py)), finds the note whose `id` equals ID (parsed with `type=int`), replaces its `text` with TEXT, saves, and prints `edited #ID`. The id and every other key are kept. The change is a pure function `edit(notes, note_id, text)` that returns the edited note or `None`.

## Alternatives considered

**Delete and re-add.** Rejected: the note would get a new id.

## Risks and mitigations

**Editing the wrong note.** Mitigation: the confirmation prints the id.

## Implementation map

- `jot/cli.py` — `edit` subcommand and `edit(notes, note_id, text)`.
- `tests/test_edit.py` — new tests.

## Acceptance criteria

- `jot edit 2 milk` leaves note 2 with text `milk` and the same id. Check: `tests/test_edit.py`
