# Design: Delete notes

Status: draft

## Problem

A mistyped note stays in jot forever; people want to remove it.

## Goals and non-goals

Goals: `jot delete ID` removes one note.

Non-goals: undo, bulk delete.

## Technical plan

`jot delete ID` loads `notes.json` ([jot/cli.py](../../jot/cli.py)), removes the note whose `id` equals ID, and saves. If no note has that id it prints `no note #ID` and exits 1. `add` keeps assigning ids as it does today.

## Alternatives considered

**Mark notes as deleted instead of removing them.** Rejected: the file would keep growing with hidden notes.

## Risks and mitigations

**Deleting the wrong note.** Mitigation: `delete` prints the removed note's text.

## Implementation map

- `jot/cli.py` — `delete` subcommand and a `delete(notes, note_id)` helper.
- `tests/test_delete.py` — new tests.

## Acceptance criteria

- `jot delete 2` removes note 2 and leaves the others. Check: `tests/test_delete.py`
- `jot delete 99` prints `no note #99` and exits 1. Check: `tests/test_delete.py`
