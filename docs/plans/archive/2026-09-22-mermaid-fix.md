## PR Metadata
- **Type**: docs
- **Research**: GitHub's diagram renderer crashes on reserved subgraph ids (`in`, `out`) and on `<br/>` inside node labels. That is the "reading 'render'" error.
- **Research summary**: Rewrite the flowchart with plain labels and subgraph ids that are not Mermaid keywords. The paragraph under the diagram still carries the detail.
- **Evidence**: docs only
- **Commit**: docs: make the architecture diagram render on GitHub
- **Files changed**: README.md
- **Design docs**: README.md

# HARNESS Plan: Mermaid render fix

| Field | Value |
|-------|-------|
| Type | docs |
| Status | build-complete |
| Short slug | mermaid-fix |
| Created | 2026-09-22 |
| Branch | feature/tui-red-bids |

## Steps

### 1. Replace the diagram — `done`
- **File(s):** README.md
- **Action:** Remove `<br/>` and the subgraph ids `in` and `out`. Keep the same path from Binance and the fixture through the adapter, TCP, feedd, the ring, and the readers.
- **Acceptance:** The mermaid block has no HTML line breaks and no reserved subgraph id.
