## PR Metadata
- **Type**: docs
- **Research**: none. The README describes the code already on dev. The picture is the window the operator sent.
- **Research summary**: No new design. The page has to be honest that the still shows the old crossed book, and that the current code replaces each top-of-book picture.
- **Evidence**: make test ok; make e2e bids=2 asks=2
- **Commit**: docs: add an internship README with the live window
- **Files changed**: README.md, docs/images/tui.png
- **Design docs**: README.md, docs/design/core-architecture.md

# HARNESS Plan: Internship README

| Field | Value |
|-------|-------|
| Type | docs |
| Status | build-complete |
| Short slug | internship-readme |
| Created | 2026-09-21 |
| Branch | feature/tui-red-bids |

## Steps

### 1. README and picture — `done`
- **File(s):** README.md, docs/images/tui.png
- **Action:** Replace the short README with a page an interviewer can skim. Use the operator's window still, and say that still is from before the picture-replace fix.
- **Acceptance:** The image is in the repo and the README links to it. The page does not call this a matching engine.
