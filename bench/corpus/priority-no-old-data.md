# Design: Priority

Status: draft

## Problem

People cannot tell urgent notes from the rest.

## Goals and non-goals

Goals: mark a note's priority; filter by it.

Non-goals: sorting, reminders.

## Technical plan

Each note gains a `priority` field (`high`, `normal` or `low`), stored in `notes.json` next to `id` and `text` (see [jot/cli.py](../../jot/cli.py)). `jot add TEXT --priority high` sets it; the default is `normal`. `jot list --priority high` prints only notes with that priority, through a pure function `filter_by_priority(notes, level)`.

## Alternatives considered

**Order notes by priority instead of filtering.** Rejected: people asked to hide low-priority notes, not to sort them.

## Risks and mitigations

**Typos in the level.** Mitigation: argparse `choices` rejects anything else.

## Implementation map

- `jot/cli.py` — `priority` on notes, `--priority` on add and list, `filter_by_priority`.
- `tests/test_priority.py` — tests.

## Acceptance criteria

- `jot list --priority high` prints only high-priority notes. Check: `tests/test_priority.py`
