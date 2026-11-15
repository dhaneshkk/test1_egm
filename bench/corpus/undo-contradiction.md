# Design: Undo the last add

Status: draft

## Problem

A mistyped `jot add` can only be removed by editing `notes.json`.

## Goals and non-goals

Goals: `jot undo` removes the most recently added note.

Non-goals: undoing edits or deletes, multi-level history beyond repeated undo.

## Technical plan

`jot undo` loads notes ([jot/cli.py](../../jot/cli.py)), removes the note with the highest `id`, saves, and prints `removed #ID`. With no notes it prints `nothing to undo` to stderr and exits 1. Repeating `undo` removes the next highest. No history file is kept: the highest id is the latest add, because ids count up from 1.

## Alternatives considered

**A history file.** Rejected: it can disagree with `notes.json`.

## Risks and mitigations

**Undoing by accident.** Accepted: the confirmation prints the id.

## Implementation map

- `jot/cli.py` — `undo` subcommand.
- `tests/test_undo.py` — new tests.

## Acceptance criteria

- With notes 1-3, `jot undo` removes note 1 and prints `removed #1`. Check: `tests/test_undo.py`
- With no notes, `jot undo` exits 1 and prints `nothing to undo` to stderr. Check: `tests/test_undo.py`
