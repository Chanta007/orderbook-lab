# Verify Step Registry

> Status: stub — populate after the first verify run or copy from a working project

## Intended Structure

Each verify step will be one entry with these fields:

- `id`: short-kebab unique identifier (e.g., `tests-run`, `lint-clean`).
- `description`: one-line human-readable summary.
- `type`: one of `shell-command`, `file-diff`, `data-check`, `compliance-rule`.
- `applies_when`: predicate for whether this step runs (e.g., `always`, `python_changed`, `plugin_changed`, `skill_modified`).

### Type-specific contracts

| Type | Fields | Example |
|------|--------|---------|
| `shell-command` | `command`, `expected_exit`, `output_regex`, `timeout_s` | Run `npm test`, expect exit 0 |
| `file-diff` | `pattern`, `scope`, `must_match` | Grep changed files for `console.log`, must NOT match |
| `data-check` | `predicate`, `args` | Named function in `verify_runner.py` |
| `compliance-rule` | `constraint_section`, `assertion`, `evidence_check` | Link a CONSTRAINTS §X.Y to a programmatic check |

## Dispatch order

Steps run in declared order. The orchestrator collects `{id, status: pass|fail|skip, duration_ms, output_excerpt}` per step and prints a JSON summary to stdout.
