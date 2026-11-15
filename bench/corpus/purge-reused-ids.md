# Design: Purge done notes

Status: draft

## Problem

Finished notes pile up and hide the open ones.

## Goals and non-goals

Goals: `jot purge` drops finished notes.

Non-goals: archiving, undo.

## Technical plan

`jot purge` loads notes ([jot/cli.py](../../jot/cli.py)), removes every note whose text starts with `[done]`, saves, and prints how many it removed. It prints `nothing to purge` when none match. `add` keeps assigning ids as it does today.

## Alternatives considered

**Hide done notes in `list`.** Rejected: the file keeps growing.

## Risks and mitigations

**Purging the wrong notes.** Mitigation: it prints the count.

## Implementation map

- `jot/cli.py` — `purge` subcommand and `purge_done(notes)`.
- `tests/test_purge.py` — tests.

## Acceptance criteria

- `jot purge` removes `[done]` notes and keeps the rest. Check: `tests/test_purge.py`
