# Telemetry Schema v1

> Locked: 2026-04-25
> Future changes require `v: 2` and a documented dual-write window.

Each line of `docs/telemetry/runs.jsonl` is one JSON object. The file is append-only, repo-tracked, and committed to git. No external services.

## Required fields

| Field | Type | Null? | Description |
|-------|------|-------|-------------|
| `v` | `integer` | No | Schema version. Always `1` for this file. Future additions require `v: 2`. |
| `run_id` | `string` (UUID4) | No | Stable run identity. Generated fresh per write via `uuid.uuid4()`. Used as the natural join key for deduplication and cross-record analysis. Beats implicit composite key (timestamp + task_slug) which is unstable across re-runs and clock skew. |
| `ts` | `string` (ISO 8601 UTC) | No | Write timestamp. `datetime.now(timezone.utc).isoformat()`. |
| `model_id` | `string` | No | The actual session model, normalized. Provider prefixes stripped by `router.py` before write. Examples: `claude-opus-4-7` (unchanged), `kimi-k2p6` (from `accounts/fireworks/models/kimi-k2p6`), `gemini-2.5-pro` (from `models/gemini-2.5-pro`). Analyzers must treat this as a flat identifier, not assume Anthropic-only patterns. |
| `model_id_source` | `string` | No | One of: `"prompt_hook"` (captured from UserPromptSubmit payload), `"self_report"` (LLM self-identified, fallback), `"unknown"` (field absent or empty). |
| `task_slug` | `string` | No | From `.claude/session-state.json` `task_slug`. Empty string if absent. |
| `skill` | `string` | No | Skill that triggered the write. `"verify"`, `"build"`, `"do"`, etc. |
| `plan_archive` | `string \| null` | Yes | Path to archived plan, e.g. `docs/plans/archive/2026-04-25-foo.md`. `null` when no plan archive exists (e.g. `do` skill or aborted plan). |
| `plan_type` | `string \| null` | Yes | One of: `feature`, `bugfix`, `refactor`, `chore`, `docs`, `test`, `infrastructure`, `data-research`. Reads `Type` from plan frontmatter. `null` when no plan or unparseable. |
| `plan_size` | `string \| null` | Yes | `small` (<=3 steps), `medium` (4-10 steps), `large` (11+ steps). Derived from step count in plan archive. `null` when uncomputable. |
| `plan_risk` | `string \| null` | Yes | `low`, `medium`, `high`. Reads `Risk` from plan frontmatter if present. `null` if absent or unparseable. |
| `branch` | `string` | No | Git branch at write time. `"unknown"` if outside a repo or detached HEAD. |
| `commit_hash` | `string` | No | `git rev-parse HEAD` at write time. Links record to specific repo state for replaying analyses against historical archive contents. `"unknown"` if unresolvable. |
| `constraints_referenced` | `array<string>` | No | Deduplicated `§X.Y` citations found in the active plan markdown via regex `§[A-Za-z0-9.]+`. Empty array if no plan or none found. This is the denominator for "earns its keep" analysis. |
| `constraints_violated` | `array<string>` | No | Deduplicated `§X.Y:check-name` strings for checks that failed during verify. Empty array if all passed or not a verify run. |
| `verify_steps_run` | `array<object>` | Yes | Per-step results from verify. Each object: `{id: string, status: "pass"|"fail"|"skip", duration_ms: integer}`. `null` when not a verify run or data unavailable. |
| `fixes_count` | `object` | Yes | `{minor: integer, major: integer}` from build phase. `null` when not a build/verify run or unrecorded. |
| `duration_ms` | `integer` | No | Wall-clock milliseconds for the phase that wrote this record (verify, build, etc.). `-1` if unmeasurable. |
| `tokens` | `object` | Yes | `{input: integer|null, output: integer|null}`. `null` when unavailable (CLI telemetry has no token access). |

## Field inclusion rationale for deep-mode queries

- **`model_id` + `model_id_source`**: Segment all analyses by actual session model. Source distinguishes reliable (`prompt_hook`) from degraded (`unknown`) data.
- **`constraints_referenced` vs `constraints_violated`**: Compute "referenced but never violated" per model → prune candidates.
- **`verify_steps_run` + `duration_ms`**: Identify slow steps that never fail (`pass_rate == 1.0`, high mean duration).
- **`plan_type` / `plan_size` / `plan_risk`**: Correlate violation patterns with plan characteristics.
- **`commit_hash`**: Re-run historical analysis against exact repo state.
- **`run_id`**: Stable deduplication across re-imports, log rotations, and partial writes.

## Schema migration policy

1. `v: 1` is locked. No new required fields may be added to v1.
2. When a new field is needed, create `SCHEMA-v2.md` alongside this file.
3. The telemetry writer must dual-write both `v: 1` and `v: 2` for a full calendar month.
4. After the dual-write window, `telemetry.py validate` may reject v1 records that predate the window, but must accept v1 records from within it.
5. The `v` field is the single discriminator; analyzers branch on it.
