# Design: Purge done notes

Status: draft

## Problem

Finished notes pile up and hide the open ones.

## Goals and non-goals

Goals: `jot purge` drops finished notes.

Non-goals: archiving, undo.

## Technical plan

`jot purge` loads notes ([jot/cli.py](../../jot/cli.py)), removes every note whose text starts with `[done]`, saves, and prints how many it removed. It prints `nothing to purge` when none match. Because `add` assigns `len(notes) + 1`, a purge would let a new note take a live id; `add` changes to `max((n["id"] for n in notes), default=0) + 1`, so a new note never takes the id of a note that is still there. An id freed by purging the newest note can come back; that is accepted, because ids only have to tell live notes apart.

## Alternatives considered

**Hide done notes in `list`.** Rejected: the file keeps growing.

**Renumber after a purge.** Rejected: ids people remember would change.

## Risks and mitigations

**Purging the wrong notes.** Mitigation: it prints the count.

## Implementation map

- `jot/cli.py` — `purge` subcommand, `purge_done(notes)`, max-based ids in `add`.
- `tests/test_purge.py` — tests.

## Acceptance criteria

- `jot purge` removes `[done]` notes and keeps the rest. Check: `tests/test_purge.py`
- After a purge, `jot add x` gets an id no remaining note has. Check: `tests/test_purge.py`
