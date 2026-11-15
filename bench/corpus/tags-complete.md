# Design: Tags

Status: draft

## Problem

People who keep work and personal notes in jot cannot list just one kind. They want to label notes when adding them and filter by label.

## Goals and non-goals

Goals: tag a note on add; list notes carrying one tag.

Non-goals: renaming tags, multi-tag filters, case folding.

## Technical plan

Each note gains a `tags` list, stored in `notes.json` next to `id` and `text` (see [jot/cli.py](../../jot/cli.py)). `jot add TEXT --tag NAME` accepts a repeatable `--tag`; duplicates are dropped and order is kept. `jot list --tag NAME` prints only notes whose `tags` contain NAME. Matching is exact and case-sensitive. Filtering lives in a pure function `filter_by_tag(notes, tag)` in `jot/cli.py`; `render` appends ` [tag1, tag2]` when a note has tags.

Notes saved before this change have no `tags` key. Every reader uses `note.get("tags", [])`, so old notes load, list, and never match a tag filter. jot never rewrites old notes, so no migration is needed.

## Alternatives considered

**A separate tag index file.** Rejected: two files can disagree after a crash or hand edit.

**Hashtags in note text.** Rejected: `#1` already means a note id, so parsing is ambiguous.

## Risks and mitigations

**A typo splits one tag into two.** Mitigation: `render` prints tags, so a stray spelling is visible.

**Case surprises (`Work` vs `work`).** Accepted for now: matching is documented as case-sensitive.

## Implementation map

- `jot/cli.py` — `tags` on new notes, `--tag` on `add` and `list`, `filter_by_tag`, tags in `render`.
- `tests/test_tags.py` — new tests for tagging and filtering.

## Acceptance criteria

- `jot add "standup" --tag work --tag work` stores `tags: ["work"]`. Check: `tests/test_tags.py`
- `jot list --tag work` prints only notes tagged `work`. Check: `tests/test_tags.py`
- A `notes.json` written before tags existed still lists every note, and `list --tag` skips those notes without error. Check: `tests/test_tags.py`
