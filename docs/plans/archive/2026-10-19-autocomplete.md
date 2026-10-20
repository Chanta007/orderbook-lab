## PR Metadata

- **Type**: feat
- **PRs**: (filled in after the merge)
- **Merge commits**: (filled in after the merge)
- **Research**: three searches on the interaction rather than the code — the W3C ARIA combobox pattern (roles, `aria-activedescendant`, and what Tab/Escape/Enter each mean), the debounce-plus-`AbortController` rule for server-side completion, and the shell completion conventions (Tab inserts the highlighted entry, Enter runs the line).
- **Research summary**: the pattern and the shell agree on everything except Tab, which the pattern sends to the next control and a shell uses to complete. The page has one focusable control, so Tab completes — a deliberate, documented deviation. The debounce rule is "both, not either": a pause before the request *and* abort the in-flight one, because debouncing alone lets a slow answer overwrite a newer one.
- **Evidence**: `make test`, `rust-test`, `view-check`, `web-check` all PASS under `set -e -o pipefail`; 14 socket tests (six about completion), 9 browser tests (four about the dropdown), and a pty test that completes a command with Tab in the terminal.
- **Review**: the vocabulary cross-check found a three-command lie in the reference on its first run (OPT-050); the browser click test found a handler reading a cleared list (OPT-051).
- **Commit**: feat(view): one vocabulary for what can be typed
- **Files changed**: `rust/ob-view/src/command.rs` (new), `rust/ob-view/src/{lib,compose,snapshot,line}.rs`, `rust/ob-cli/src/{web,tickers,actions,window}.rs`, `rust/ob-cli/src/bin/{tui,view,web}.rs`, `python/test_web.py`, `python/test_web_browser.py`, `python/test_tui_input.py`, `docs/design/{view-layer,testing}.md`, `docs/runbooks/web-frontend.md`, `docs/optimization/{ledger.jsonl,README.md}`, `docs/VERIFY.md`, `README.md`

# Active Plan: one vocabulary for what can be typed

| Field | Value |
|-------|-------|
| Type | feat |
| Status | verified-and-merged |
| Short slug | autocomplete |
| Created | 2026-10-19 |
| Branch | feature/autocomplete |
| Worktree | .claude/worktrees/autocomplete |
| Design docs | docs/design/view-layer.md |
| PR | (filled in after the merge) |
| Goal | smartdeploy-to-dev (ship to dev directly approved by the user this turn) |

## Context

The page's input bar had no completion: typing `eth` did nothing, and the dropdown the
user asked for did not exist. In the terminal, the picker completed symbols only, so
`bo` offered nothing even though the reference lists `book`. The reference itself was a
formatted array of strings — a third list of what can be typed, and the only one that
was not executable.

## Decisions

- **One vocabulary in `ob_view::command`**: the commands, the three times and the two
  key rows, with the reference, the picker, the dropdown and the terminal's Tab all
  built from it.
- **The page asks the server.** Completing in JavaScript would be a second
  implementation of what a prefix means, in a second language.
- **The dropdown lists what the reference lists, in the reference's order**, so `sta`
  offers `status` before `staleness`.
- **The interaction is the shell's**, with the ARIA combobox roles the pattern asks
  for. Tab completes, which is a documented deviation from the pattern: the bar is the
  page's only focusable control and the convention for a typed line is a shell's.
- **Debounced and aborted**: 120 ms, then abort the in-flight request when the line
  moves on.
- **The three times became commands**, because the reference has claimed they were
  since the header gained those numbers.

## Scope

| Step | Deliverable | Done when |
|------|-------------|-----------|
| 1 | The vocabulary, the reference built from it, and the completion | 64 unit tests pass, including the reference's order and alignment |
| 2 | The terminal's picker and Tab on the shared completion | A pty test completes `sta` to `status` with Tab and runs it |
| 3 | `GET /complete` and the page's dropdown | 6 socket tests, 4 browser tests |
| 4 | The cross-check that the reference and the dispatcher agree | It fails on a command the dispatcher does not know — and did, on `lag`, `staleness`, `age` |
| 5 | Docs, ledger, sweep | OPT-049 to OPT-051 recorded, the sweep green |

## Non-goals

- Completing inside a quoted argument, or shell-style file paths.
- A completion for the terminal's number argument (`levels N`): a number is typed.
- Any JavaScript dependency: the dropdown is DOM, aria attributes and forty lines.

## Verification

`make test`, `make rust-test`, `make view-check`, `make web-check` — each run directly
under `set -e -o pipefail`, all PASS. The completion is covered from three directions:
the vocabulary's own unit tests, the socket test of `/complete`, and the browser test
that types, tabs, arrows, escapes and clicks.
