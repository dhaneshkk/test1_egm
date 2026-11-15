# Design: Export to CSV

Status: draft

## Problem

People want their notes in a spreadsheet.

## Goals and non-goals

Goals: `jot export-csv FILE` writes every note to FILE as CSV.

Non-goals: importing CSV, choosing columns.

## Technical plan

`jot export-csv FILE` loads notes ([jot/cli.py](../../jot/cli.py)) and writes FILE: a header row `id,text`, then one row per note in id order. FILE is overwritten if it exists. It prints `exported N notes to FILE`.

## Alternatives considered

**JSON export.** Rejected: `notes.json` already is JSON; spreadsheets want CSV.

## Risks and mitigations

**Overwriting a file by mistake.** Accepted: the confirmation names the file.

## Implementation map

- `jot/cli.py` — `export-csv` subcommand.
- `tests/test_csv.py` — new tests.

## Acceptance criteria

- An existing FILE is replaced. Check: `tests/test_csv.py`
