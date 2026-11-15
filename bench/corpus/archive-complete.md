# Design: Archive notes

Status: draft

## Problem

Old notes clutter `jot list`, but people do not want to lose them.

## Goals and non-goals

Goals: `jot archive ID` moves a note to `archive.json`; `jot list --archived` lists archived notes.

Non-goals: restoring from the archive, archiving by date.

## Technical plan

`jot archive ID` removes the note from `notes.json` and appends it, unchanged, to `archive.json` next to it (created on first use; absent means empty), then prints `archived #ID`. An unknown ID prints `no note #ID` to stderr and exits 1. Archived notes keep their id, and `jot add` never reuses one: a new note's id is one more than the highest id in `notes.json` and `archive.json` together, replacing `len(notes) + 1` in `add` ([jot/cli.py](../../jot/cli.py)). `jot list --archived` prints archived notes in id order.

## Alternatives considered

**A flag on the note.** Rejected: `notes.json` would keep growing.

## Risks and mitigations

**The two files disagree after a crash.** Mitigation: `archive.json` is written before `notes.json`, so a crash can only leave a note in both, never in neither.

## Implementation map

- `jot/cli.py` — `archive` subcommand, `--archived` on `list`, id rule in `add`.
- `tests/test_archive.py` — new tests.

## Acceptance criteria

- After `jot archive 2`, `jot list` omits note 2 and `jot list --archived` prints it. Check: `tests/test_archive.py`
- With notes 1-3 and note 3 archived, `jot add x` gets id 4. Check: `tests/test_archive.py`
