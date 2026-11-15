# Design: Export notes

Status: draft

## Problem

People want their notes outside jot.

## Goals and non-goals

Goals: export notes.

Non-goals: import.

## Technical plan

Add an export command that writes the notes in a suitable format to a file. Handle errors appropriately.

## Alternatives considered

**Copy notes.json by hand.** Rejected: not friendly.

## Risks and mitigations

**Large files.** Mitigation: handle efficiently.

## Implementation map

- `jot/cli.py` — export command.
- `tests/test_export.py` — tests.

## Acceptance criteria

- Export works. Check: `tests/test_export.py`
