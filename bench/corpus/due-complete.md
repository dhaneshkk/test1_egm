# Design: Due dates

Status: draft

## Problem

Notes that are to-dos have no deadline, so people cannot see what is due soon.

## Goals and non-goals

Goals: `jot add TEXT --due DATE` stores a due date; `jot list --due-before DATE` prints notes due before DATE.

Non-goals: reminders, time of day, time zones.

## Technical plan

Dates are ISO `YYYY-MM-DD`, parsed with `datetime.date.fromisoformat`; anything else is rejected by argparse with exit 2. `jot add TEXT --due DATE` stores `due: "YYYY-MM-DD"` on the note in `notes.json` ([jot/cli.py](../../jot/cli.py)). `jot list --due-before DATE` prints, in id order, notes whose `due` is strictly earlier than DATE. Notes without `due`, including old ones, never match. `render` appends ` (due YYYY-MM-DD)` when a note has one. Filtering is a pure function `due_before(notes, date)`.

## Alternatives considered

**Free-text dates ("next friday").** Rejected: ambiguous and needs a parser dependency.

## Risks and mitigations

**Typos in dates.** Mitigation: invalid dates are rejected on add.

## Implementation map

- `jot/cli.py` — `--due` on `add`, `--due-before` on `list`, `due_before`, `render`.
- `tests/test_due.py` — new tests.

## Acceptance criteria

- `jot add pay --due 2026-03-01` stores `due: "2026-03-01"`. Check: `tests/test_due.py`
- `jot list --due-before 2026-03-02` prints that note; `--due-before 2026-03-01` does not. Check: `tests/test_due.py`
- `jot add x --due tomorrow` exits 2 and adds nothing. Check: `tests/test_due.py`
