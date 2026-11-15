# Design: Pin notes

Status: draft

## Problem

Important notes get buried under newer ones in `jot list`.

## Goals and non-goals

Goals: `jot pin ID` marks a note pinned; `jot list` prints pinned notes first.

Non-goals: unpinning several at once, pin order by date.

## Technical plan

Each note may carry `pinned: true` in `notes.json` ([jot/cli.py](../../jot/cli.py)). `jot pin ID` sets it and prints `pinned #ID`; pinning a pinned note is a no-op. `jot unpin ID` removes the key. An unknown ID prints `no note #ID` to stderr and exits 1. `jot list` prints pinned notes first, then the rest; within each group notes keep ascending id order. Ordering is a pure function `pinned_first(notes)`.

## Alternatives considered

**A separate pins file.** Rejected: two files can disagree.

## Risks and mitigations

**Users expect pins at the bottom.** Accepted: the README says pinned notes come first.

## Implementation map

- `jot/cli.py` — `pin` and `unpin` subcommands, `pinned_first`, `list` ordering.
- `tests/test_pin.py` — new tests.

## Acceptance criteria

- After `jot pin 3`, `jot list` prints note 3 first, then the others in id order. Check: `tests/test_pin.py`
- `jot pin 9` with no note 9 exits 1. Check: `tests/test_pin.py`
