# Deferred Items — Central Index

<!-- ENTRIES BELOW -->

---
id: 14157259cc0fe9
title: Call CTest from CI
source-path: inline:first-test-run
source-section: inline capture 2026-09-22
status: open
priority: medium
i: 3
c: 3
e: 3
revisit-when: make is unavailable on a required runner and ctest is the only launcher that runner provides
created: 2026-09-22
---

Call CTest from CI

---
id: 4c352b07a928ef
title: Paint bids green to match exchange books
source-path: inline:tui-red-bids
source-section: inline capture 2026-09-22
status: open
priority: medium
i: 3
c: 3
e: 3
revisit-when: The operator says bid prices should be green
created: 2026-09-22
---

Paint bids green to match exchange books

---
id: 815f4d5eaa2736
title: Tighten headless e2e to fixture counts
source-path: inline:first-test-run
source-section: inline capture 2026-09-22
status: open
priority: medium
i: 3
c: 3
e: 3
revisit-when: A fixture edit changes the book and make e2e still exits 0
created: 2026-09-22
---

Tighten headless e2e to fixture counts

---
id: a50f83c708f596
title: Replace factory CLAUDE.md quick start with make test/e2e
source-path: inline:first-test-run
source-section: inline capture 2026-09-22
status: open
priority: medium
i: 3
c: 3
e: 3
revisit-when: A session follows CLAUDE.md and tries npm test or docker compose, or make is run from config/ and reports no makefile
created: 2026-09-22
---

Replace factory CLAUDE.md quick start with make test/e2e

---
id: e41a1e2b10c209
title: Replace stdlib websocket client
source-path: inline:tui-red-bids
source-section: inline capture 2026-09-22
status: open
priority: medium
i: 3
c: 3
e: 3
revisit-when: A live session shows a fragmented or compressed frame ws_frames drops, or the 24-hour disconnect is not followed by a reconnect
created: 2026-09-22
---

Replace stdlib websocket client

---
id: 11a931c35554a9
title: Non-blocking stdin so Ctrl-C cannot stick
source-path: docs/research/2026-09-21-tui-red-bids.md
source-section: Parked Future Angles
status: open
priority: medium
i: 3
c: 3
e: 3
revisit-when: a SIGINT leaves the tui process alive after the feed has stopped
created: 2026-09-22
---

Non-blocking stdin so Ctrl-C cannot stick

---
id: 5cc8259902d1ab
title: Move the TUI to FTXUI
source-path: docs/research/2026-09-21-tui-red-bids.md
source-section: Parked Future Angles
status: open
priority: medium
i: 3
c: 3
e: 3
revisit-when: one macOS Terminal window cannot show the book and the command list with ANSI alone
created: 2026-09-22
---

Move the TUI to FTXUI

---
id: 326326d6bda2dc
title: `obctl status` command printing each tracked pid, liveness, and the port hold...
source-path: docs/plans/archive/2026-10-11-mac-run-path.md
source-section: ## Deferred (NOT in this plan)
status: open
priority: medium
i: 3
c: 2
e: 2
revisit-when: manual review
created: 2026-10-12
---

`obctl status` command printing each tracked pid, liveness, and the port holder. Revisit when a third "why is my window stalled" question shows up.

---
id: ad7bf482ea0d4e
title: Cross-worktree port message: when the port is held by a feedd from another ch...
source-path: docs/plans/archive/2026-10-11-mac-run-path.md
source-section: ## Deferred (NOT in this plan)
status: open
priority: medium
i: 3
c: 2
e: 2
revisit-when: manual review
created: 2026-10-12
---

Cross-worktree port message: when the port is held by a feedd from another checkout, the refusal names the address but not the other worktree. Revisit when two worktrees are worked at once.

---
id: b0095323eccb6e
title: Add a product-path OTKP pack for the order book run path
source-path: inline:adhoc
source-section: inline capture 2026-10-12
status: open
priority: medium
i: 3
c: 3
e: 3
revisit-when: when a second macOS/Linux run-path regression appears, or when the OTKP engine accepts consumer packs
created: 2026-10-12
---

Add a product-path OTKP pack for the order book run path

---
id: 4b53f8d03b1084
title: A `docs/runbooks/running-it.md` when the README `Run it` section passes rough...
source-path: docs/plans/archive/2026-10-12-run-docs-gaps.md
source-section: ## Deferred (NOT in this plan)
status: open
priority: medium
i: 3
c: 2
e: 2
revisit-when: manual review
created: 2026-10-12
---

A `docs/runbooks/running-it.md` when the README `Run it` section passes roughly 40 lines.

---
id: 5fb6d85c14fb86
title: ship-merge-guard misses a leading heredoc, so a later cd is not applied and the git verb is classified against the wrong checkout
source-path: inline:adhoc
source-section: inline capture 2026-10-12
status: open
priority: medium
i: 3
c: 3
e: 3
revisit-when: when a commit is blocked on a protected checkout while the command does cd into a feature worktree
created: 2026-10-12
---

ship-merge-guard misses a leading heredoc, so a later cd is not applied and the git verb is classified against the wrong checkout

---
id: c0bdd282db0205
title: Documenting the `obctl` exit codes (`0`, `1` on a refusal, `1` from `stop` wh...
source-path: docs/plans/archive/2026-10-12-run-docs-gaps.md
source-section: ## Deferred (NOT in this plan)
status: open
priority: medium
i: 3
c: 2
e: 2
revisit-when: manual review
created: 2026-10-12
---

Documenting the `obctl` exit codes (`0`, `1` on a refusal, `1` from `stop` when a process survives).

---
id: cc4e1affa462e1
title: ship-merge-guard resolves telemetry from the hook cwd, so a worktree branch's review2fix rows are invisible to it
source-path: inline:adhoc
source-section: inline capture 2026-10-12
status: open
priority: medium
i: 3
c: 3
e: 3
revisit-when: when a worktree delivery is blocked for missing review evidence that exists on its own branch
created: 2026-10-12
---

ship-merge-guard resolves telemetry from the hook cwd, so a worktree branch's review2fix rows are invisible to it

---
id: d0705e552a0fe6
title: smartdeploy.py accepts only result=pass while ship-merge-guard and the drive/review2fix skills treat skipped-unprovisioned as ship-OK
source-path: inline:adhoc
source-section: inline capture 2026-10-12
status: open
priority: medium
i: 3
c: 3
e: 3
revisit-when: when the next unprovisioned-reviewer delivery is blocked at the smartdeploy gate
created: 2026-10-12
---

smartdeploy.py accepts only result=pass while ship-merge-guard and the drive/review2fix skills treat skipped-unprovisioned as ship-OK
