# Design: Count notes

Status: draft

## Problem

People with many notes want to know how many they have without scrolling.

## Goals and non-goals

Goals: `jot count` prints the number of notes.

Non-goals: counts per tag, statistics.

## Technical plan

`jot count` loads notes ([jot/cli.py](../../jot/cli.py)) and prints `len(notes)` as a bare integer followed by a newline, and exits 0. An absent `notes.json` prints `0`. It reads nothing else and never writes.

## Alternatives considered

**`jot list | wc -l`.** Rejected: not obvious, and breaks if a note contains a newline.

## Risks and mitigations

**None worth noting.** It only reads the file.

## Implementation map

- `jot/cli.py` — `count` subcommand.
- `tests/test_count.py` — new tests.

## Acceptance criteria

- With three notes, `jot count` prints `3`. Check: `tests/test_count.py`
- With no `notes.json`, `jot count` prints `0` and creates no file. Check: `tests/test_count.py`
