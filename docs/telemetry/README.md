# Telemetry

This directory contains repo-tracked JSONL telemetry records.

- `runs-YYYY-MM.jsonl` — one JSON object per line, appended by the verify skill on successful commit
- `SCHEMA-v1.md` — schema documentation for the v1 record format

Telemetry is committed to git. No external services. Records are append-only; historical records are never modified or deleted.
