# Design: Show one note

Status: draft

## Problem

With many notes, finding one by id means scanning `jot list`.

## Goals and non-goals

Goals: `jot show ID` prints one note.

Non-goals: showing several ids, formatting options.

## Technical plan

`jot show ID` (ID parsed with `type=int`) loads notes ([jot/cli.py](../../jot/cli.py)) and prints `render(note)` for the note whose `id` equals ID. It never writes.

## Alternatives considered

**`jot list | grep '#ID '`.** Rejected: matches text too.

## Risks and mitigations

**None worth noting.** It only reads the file.

## Implementation map

- `jot/cli.py` — `show` subcommand.
- `tests/test_show.py` — new tests.

## Acceptance criteria

- `jot show 2` prints `#2 milk` for note 2 `milk`. Check: `tests/test_show.py`
