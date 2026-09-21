# Plan: Data Pipeline

> Skill: `/mindcoachlabs:data`

## When to Use

Data gathering, ETL, scraping, or pipeline tasks where:
- Data must be collected from multiple sources
- Interim results should be saved for re-runs without restarting the full pipeline
- Data quality, completeness, and consistency are critical
- Analysis and conclusions must be reproducible (Python-driven, not LLM-guessed)

## Track Selection

| Track | When | What's Different |
|-------|------|-----------------|
| **One-off** (default) | One-time data gathering or analysis | Standard pipeline, archive when done |
| **Continuous** | Recurring ETL, production pipelines, scheduled jobs | Adds Phase 7: scheduling, incremental extraction, design doc + runbook creation |

Keywords that trigger continuous track: "recurring", "scheduled", "incremental", "production", "ongoing", "industrial", "cron", "daily", "weekly", "monitor".

## Pipeline Stages

```
Define Contracts → Extract → Normalize → Validate → Analyze → Report
                                                                 ↓ (continuous only)
                                                            Operationalize
```

Each stage checkpoints its outputs. Re-runs skip completed stages unless inputs changed.

## Steps

### 1. Define data contracts

Before any extraction, define the expected output schema:
- Field names, types, constraints, required/optional
- This becomes the normalization target and quality check baseline
- Save as `data/<plan-slug>/contracts/schema.json`

### 2. Register data sources

Create a source registry with quality rankings:

| Tier | Source Type | Trust Level |
|------|-----------|-------------|
| 1 | Authoritative databases, official APIs | Highest |
| 2 | Public databases, government data | High |
| 3 | Web scraping, internet sources | Medium |
| 4 | LLM training data / model knowledge | Lowest — avoid |

For each source, document: URL/access method, rate limits, authentication, data format, expected volume.

### 3. Confirm assumptions

Before extraction, document and confirm all assumptions:

| # | Assumption | Status | Confirmed By | Date |
|---|-----------|--------|-------------|------|
| A1 | Example: USD currency for all prices | unconfirmed | — | — |

In interactive mode: pause and confirm with user.
In auto mode: make best guess, flag clearly as `auto-confirmed`.

### 4. Extract data

- Run extraction stages in parallel where sources are independent
- Save raw data to `data/<plan-slug>/raw/` with source metadata
- Checkpoint after each source (commit to git if <50MB, otherwise local disk + flag)
- Use LLM for unstructured data classification and extraction (prefer over regex/pattern matching)
- Log metrics: `[PIPELINE] stage=extract | source=X | status=running | records=N`

### 5. Normalize data

- Transform raw data against the defined schema/contracts
- Save to `data/<plan-slug>/normalized/`
- Flag and log: schema violations, missing fields, type mismatches, duplicates
- All transformations of unstructured data should prefer LLM models over pattern matching

### 6. Validate (3-source triangulation)

- Cross-check data from 3 independent sources or angles
- If only 2 sources available, confirm with user (auto mode: proceed with flag)
- Generate quality report: completeness %, consistency %, accuracy %
- STOP if quality below threshold (default 80%, configurable in assumptions)

### 7. Analyze

- All calculations, statistics, and conclusions MUST be Python code-driven
- Save scripts to `data/<plan-slug>/analysis/`
- LLM orchestrates and interprets results but never computes numerical outputs
- This eliminates hallucination risk for quantitative work

### 8. Report and commit

- Generate summary: findings, assumptions used, quality scores, source attributions
- Update plan file with full pipeline status
- Commit artifacts to git (flag if too large for git)
- Archive plan with PR metadata

## Data Directory Convention

```
data/<plan-slug>/
├── manifest.json       # Artifact index (checksums, timestamps, sources)
├── pipeline-status.json # Live pipeline metrics
├── contracts/          # Schema definitions
├── raw/                # Original extracts (per source)
├── normalized/         # Cleaned, schema-conformant data
├── analysis/           # Python scripts + outputs
└── reports/            # Final outputs and summaries
```

## Environment

**Preference order:**
1. Docker container (isolation, reproducibility) — mount `data/` as volume
2. Python venv with `requirements.txt` (if Docker unavailable)

Always pin dependency versions for reproducibility.

## Progress and Status

Maintain `data/<plan-slug>/pipeline-status.json`:
```json
{
  "stages": [
    { "name": "extract", "status": "complete", "progress": "5/5",
      "elapsed": "2m14s", "errors": 0, "records": 1247 }
  ],
  "assumptions": { "confirmed": 4, "unconfirmed": 1 },
  "quality": { "completeness": 0.94, "consistency": 0.87, "accuracy": 0.91 }
}
```

## Git Strategy

- Commit data to git if total size <50MB
- If >50MB: keep on local disk, log paths in manifest.json, flag to user
- Always commit: manifest.json, pipeline-status.json, contracts/, analysis/ scripts
- Never commit: credentials, API keys, auth tokens

## Phase 7: Operationalize (Continuous Track Only)

For pipelines classified as continuous, add these steps after the report:

### 9. Create design doc
- Create `docs/design/data-<slug>.md` with: purpose, key files, architecture (data flow), runtime flow, configuration, cross-references
- This makes the pipeline a documented part of the system architecture

### 10. Create runbook
- Create `docs/runbooks/data-<slug>.md` with: common failures, recovery steps, monitoring guidance, contacts

### 11. Add incremental extraction
- Create `data/<slug>/state.json` with: last_run timestamp, cursor/offset, total records
- Update extraction scripts to fetch only new/changed data on re-runs
- Add merge logic for appending to existing normalized data

### 12. Configure scheduling
- Create cron entry, GitHub Actions workflow, or document manual re-run command
- Add data freshness monitoring (alert if last_run exceeds expected interval)

## Related Docs

- **[HARNESS.md](../HARNESS.md)** — Code change workflow (§5)
- **[CONSTRAINTS.md](../CONSTRAINTS.md)** — Architectural rules
- **[design/mindcoachlabs-harness.md](../design/mindcoachlabs-harness.md)** — Skills inventory
- **[design/hooks-routing.md](../design/hooks-routing.md)** — Hooks and routing architecture
