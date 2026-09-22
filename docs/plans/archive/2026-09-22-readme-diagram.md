## PR Metadata
- **Type**: docs
- **Research**: none. GitHub renders a mermaid block in a README. Cropping the screenshot removes the window title. No product behavior changes.
- **Research summary**: The picture stays the same terminal frame, without the macOS title that showed a username. The diagram is the process path already described in the README and in docs/design/core-architecture.md.
- **Evidence**: docs only. make test and make e2e were not re-run because no program changed.
- **Commit**: docs: crop the window title and add an architecture diagram
- **Files changed**: README.md, docs/images/tui.png
- **Design docs**: README.md, docs/design/core-architecture.md

# HARNESS Plan: README picture and diagram

| Field | Value |
|-------|-------|
| Type | docs |
| Status | build-complete |
| Short slug | readme-diagram |
| Created | 2026-09-22 |
| Branch | feature/tui-red-bids |

## Steps

### 1. Crop the title bar — `done`
- **File(s):** docs/images/tui.png
- **Action:** Cut off the light window title so the username is not in the picture.
- **Acceptance:** The image starts on the black terminal text.

### 2. Architecture diagram — `done`
- **File(s):** README.md
- **Action:** Replace the short text stack with a mermaid diagram of the feed, the ring, and the readers, plus one paragraph that walks one update.
- **Acceptance:** The README contains a mermaid flowchart and does not name the operator.
