# Design: Import notes from a text file

Status: draft

## Problem

People moving from a plain text list must add each line by hand.

## Goals and non-goals

Goals: `jot import FILE` adds one note per line of FILE.

Non-goals: importing JSON or CSV, de-duplication.

## Technical plan

`jot import FILE` reads the notes in FILE and adds each one with `add` ([jot/cli.py](../../jot/cli.py)), so ids continue from the existing notes. It saves once and prints `imported N notes`. A missing FILE prints `cannot read FILE` to stderr and exits 1 with nothing added.

## Alternatives considered

**One `jot add` per line in a shell loop.** Rejected: slow and quoting breaks.

## Risks and mitigations

**A huge file.** Accepted: jot holds all notes in memory already.

## Implementation map

- `jot/cli.py` — `import` subcommand.
- `tests/test_import.py` — new tests.

## Acceptance criteria

- Importing a file adds its notes with the next ids. Check: `tests/test_import.py`
- A missing FILE exits 1 and adds nothing. Check: `tests/test_import.py`
