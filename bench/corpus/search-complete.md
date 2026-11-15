# Design: Search notes

Status: draft

## Problem

With many notes, people scroll to find one about a topic.

## Goals and non-goals

Goals: `jot search WORD` prints notes whose text contains WORD.

Non-goals: regex, ranking, fuzzy matching.

## Technical plan

`jot search WORD` loads notes ([jot/cli.py](../../jot/cli.py)) and prints, with `render`, every note whose `text` contains WORD, compared case-insensitively with `str.casefold()`. No match prints nothing and exits 0. The filter is a pure function `search(notes, word)`.

## Alternatives considered

**Reuse `list` with a `--grep` flag.** Rejected: `search` reads better and keeps `list` simple.

## Risks and mitigations

**Slow on huge files.** Accepted: jot already loads the whole file for `list`.

## Implementation map

- `jot/cli.py` — `search` subcommand and `search(notes, word)`.
- `tests/test_search.py` — new tests.

## Acceptance criteria

- `jot search milk` prints notes containing `Milk` or `milk`. Check: `tests/test_search.py`
- `jot search zzz` with no match prints nothing and exits 0. Check: `tests/test_search.py`
