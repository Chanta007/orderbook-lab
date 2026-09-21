# Research Records

Each file in this directory captures the **journey** behind a non-trivial decision: option space, evidence, and parked angles. Plans capture the **action**; design docs capture the **steady state**. Research is upstream of plan.

## When to write one

- More than two plausible options exist for a concrete codebase change.
- The decision rests on external evidence (papers, vendor docs, benchmarks) that should outlive the conversation.
- A future re-evaluation will benefit from seeing what was rejected and why.

Triggered explicitly via `/mindcoachlabs:mcl research <topic>`, or auto-suggested when `/mindcoachlabs:plan` Step 4b detects >2 plausible options.

## Wide vs deep-dive (v2.32.0+)

- **Wide research** — option space (which strategy). Default soft budget ~5 minutes (+ one pre-approved +5m when scope is large).
- **Deep-dive** — after a recommendation, optional second pass on **how to implement** the chosen option. Requires `**Predecessor**:` linking the wide file. Entry: approval reply `deep-dive`/`dive`, or `/mcl research deep-dive: <option> from docs/research/<file>.md`.
- Approval choices after wide present: `deep-dive` | `yes` | `modify` | `cancel` | `auto` | `auto2pr`.
- Prefer deep-dive when Cost=large or open implementation questions remain; never force.

## File naming

`<YYYY-MM-DD>-<kebab-slug>.md` — e.g. `2026-05-09-rag-hybrid-attention.md`.

## Schema

```markdown
# Research: <Topic>

**Predecessor**: [link to prior research file]  ← optional; only for chained research

## Question
<One paragraph stating the concrete decision being made.>

## Thinking — How We Got Here

### Initial framing & gut intuition
<What the researcher believed at the start. 1–2 paragraphs.>

### Pivots during research
<Numbered or named pivots where evidence changed direction. Each pivot must cite its
trigger (user pushback, codebase audit finding, external paper, vendor change). 2–4
pivots typical.>

### Options we considered and rejected
<DISTINCT FROM PARKED. Each rejected option includes the reason it should not be
re-proposed unless the rejection conditions change. Cite governance rules
(HARNESS §X) where relevant.>

### Open empirical questions deliberately not answered here
<Things the research couldn't answer without empirical testing. Listed explicitly so
build-phase work knows what to validate.>

## Options Considered

| Option | Pros | Cons | Cost |
|--------|------|------|------|
| <name> | … | … | … |

## Decision + Rationale

**Chosen**: <option name or sequence>

<Why this option, in 2–5 sentences. Reference evidence below.>

## Evidence

### External
- <Source name> — <URL>
  > <Verbatim extract relevant to the decision.>

### Internal patterns referenced
- `<path:line>` — <one-sentence what it does today>

### Primary sources (optional — omit the heading entirely if none)
- Official spec or package-registry URL cited before Cost=medium/large. See research skill `references/inventory.md`.

### Source tensions (optional — omit the heading entirely if zero tensions)
- **<sub-question>** — A: <position> (URL) ↔ B: <position> (URL)

## Parked Future Angles

Each entry MUST include a `*Revisit when:*` trigger.

- **<angle name>.** *Revisit when:* <concrete observable condition>
- **<angle name>.** *Revisit when:* <concrete observable condition>

## Re-evaluation Hooks

Concrete metric thresholds that should reopen this file:

- **<hook name>.** If <metric> <threshold> → <action>
- **<hook name>.** If <metric> <threshold> → <action>
```

## Schema discipline

- **`Thinking — How We Got Here` is required, not optional.** If the four subsections (initial framing, pivots, rejected options, open empirical questions) would all be empty or trivially short, the topic likely doesn't warrant a research file — skip straight to `/mindcoachlabs:mcl plan` instead.
- **Rejected vs Parked is a load-bearing distinction.**
  - **Rejected** = ruled out with explicit reasons. Should NOT be re-proposed unless those rejection conditions change. Lives in two places by design: a one-line rejection reason in the Options Considered table (Cons cell), and a longer narrative in the Thinking → "Options we considered and rejected" subsection (the *how we ruled it out* story).
  - **Parked** = a viable alternative we set aside, with a concrete `*Revisit when:*` trigger. Vague triggers like *"if performance issues"* are disallowed — every parked entry must point at an observable condition (a metric, a vendor change, a scale threshold).
- **`Re-evaluation Hooks` make the research file a forward-looking support doc.** When an alert fires six months from now, the on-call engineer can search `docs/research/` for the threshold and find the reasoning that produced the chosen option. Hooks differ from Parked in *action*: Parked = "consider an alternative we shelved"; Hook = "reopen the entire decision".
- **Predecessor link** is optional and only for chained research. Omit the line entirely when there is no predecessor — don't write `none`.
- **`### Source tensions` is an optional subsection of Evidence.** Use it when external sources disagree on a sub-question from your coverage matrix — pair the disagreeing positions with URLs so a later reader can see both sides. If no genuine tensions surfaced, omit the heading entirely (don't write "none" — it preserves visual signal that contested points are scarce in this domain).
- **`### Primary sources` is an optional subsection of Evidence.** Use it when Cost is medium/large or an option implies a new protocol/client/bridge — list official spec or registry URLs. Omit the heading if all options are Cost=small with no protocol work.

## Lifecycle

Append-only. Once a research file is written and referenced from a plan archive's `**Research**:` PR-Metadata field, it is not edited. Re-evaluation produces a NEW research file that may cite the original.

Stale-research detection (research file >90 days old, never referenced by any `docs/plans/archive/*.md`) is surfaced by `/mindcoachlabs:health-check` for review — never auto-archived.

See `docs/HARNESS.md` §2.7 for governance.
