# Design: Mark notes done

Status: draft

## Problem

Finished to-dos stay in `jot list` forever.

## Goals and non-goals

Goals: `jot done ID` marks a note done; `jot list` hides done notes; `jot list --all` shows them.

Non-goals: undoing done, due dates.

## Technical plan

`jot done ID` sets `done: true` on the note in `notes.json` ([jot/cli.py](../../jot/cli.py)) and prints `done #ID`. An unknown ID prints `no note #ID` to stderr and exits 1. `jot list` skips notes whose `done` is true; `jot list --all` prints every note, with ` (done)` after a done note's text. Notes without a `done` key, including old ones, are not done. Filtering is a pure function `visible(notes, show_all)`.

## Alternatives considered

**Delete finished notes.** Rejected: people want a record.

## Risks and mitigations

**A hidden note is forgotten.** Mitigation: `--all` shows everything.

## Implementation map

- `jot/cli.py` — `done` subcommand, `--all` on `list`, `visible`.
- `tests/test_done.py` — new tests.

## Acceptance criteria

- After `jot done 1`, `jot list` still prints note 1. Check: `tests/test_done.py`
- `jot list --all` prints note 1 followed by ` (done)`. Check: `tests/test_done.py`
- Notes saved without a `done` key are listed. Check: `tests/test_done.py`
