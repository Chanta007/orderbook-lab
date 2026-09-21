# CLAUDE.md

**Factory-seeded brief** (cli-tool): Low-latency C++ order-book lab for macOS and Ubuntu: multithreaded market-data feed process, generic in-memory bus, WAL point-in-time recovery, OpenTelemetry trace IDs off the hot path, terminal UI, file-driven setup/start/stop CLI, Binance-style test crypto feed. Dev vs prod isolation. Default branch dev, production main.

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

> **This is a condensed quick reference.** Full architectural rules: `docs/CONSTRAINTS.md`. Documentation governance and templates: `docs/HARNESS.md`.

## Quick Start Commands

```bash
# Development
# npm run dev / python manage.py runserver / go run main.go

# Build
# npm run build / docker build -t myapp .

# Test
# npm test / pytest / go test ./...

# Lint
# npm run lint / ruff check . / golangci-lint run

# Type check (if applicable)
# npx tsc --noEmit / mypy .
```

**Docker (recommended)**:
```bash
docker compose up --build   # Full stack at localhost:3000 (or your port)
```

**Before Committing**:
1. Run tests
2. Run type checker (if applicable)
3. Run linter
4. Run build
5. Update relevant `docs/design/*.md` if architecture/data models/flows changed

**Git Workflow**: Work on feature branches. PRs target `dev`. For parallel work: `python launch_worktree.py {name}` (creates worktree, copies `.env` files).

## Architecture Overview

### Documentation

| Doc | Purpose |
|-----|---------|
| `docs/CONSTRAINTS.md` | Architectural rules (the single source of truth) |
| `docs/design/*.md` | Domain-specific design specs |
| `docs/plans/*.md` | Step-by-step execution plans for common tasks |
| `docs/runbooks/*.md` | Debugging playbooks for known failure patterns |
| `docs/HARNESS.md` | Documentation governance, templates, full doc index |

**Reading order for a task**: CLAUDE.md (auto-loaded) → Source-to-doc mapping below → relevant `design/*.md` → `CONSTRAINTS.md` section → `plans/*.md` if applicable.

### Task Navigation

| I want to... | Read |
|--------------|------|
| Add a new API endpoint | `plans/add-api-route.md` then `design/api-design.md` |
| Add test coverage | `plans/add-test.md` then `design/testing.md` |
| Add a new feature | `plans/add-feature.md` then relevant design doc |
| Modify the database schema | `plans/add-data-model.md` then `design/data-model.md` |
| Add observability | `plans/add-observability.md` then `design/observability.md` |
| Integrate an external service | `plans/add-integration.md` then `design/api-design.md` |
| Debug production errors | `design/observability.md` then `runbooks/` |
| Understand auth/RBAC | `design/auth-rbac.md` |
| Fix or add mobile UX | `design/ui-ux.md` then `CONSTRAINTS.md` §10 |
| Work on parallel features | use `python launch_worktree.py` |
| Start any task (gateway routes to right skill) | `/mindcoachlabs:mcl <describe what you want>` |
| Research options before planning a non-trivial choice | `/mindcoachlabs:mcl research <topic>` |
| Use MindCoachLabs workflow (plan/build/verify) | `/mindcoachlabs:plan` → `/mindcoachlabs:build` → `/mindcoachlabs:verify` |
| Run a data research pipeline | `/mindcoachlabs:data <task>` then `plans/data-research.md` |
| Modify an active plan mid-build | `/mindcoachlabs:plan modify: <what to change>` |
| Diagnose harness issues | `/mindcoachlabs:health-check` or `/mindcoachlabs:health-check --fix` |

### Source File to Design Doc Mapping

- Authentication / RBAC → `docs/design/auth-rbac.md`
- Database models → `docs/design/data-model.md`
- API routes → `docs/design/api-design.md`
- UI components → `docs/design/ui-ux.md`
- Tests → `docs/design/testing.md`
- Observability / logging → `docs/design/observability.md`
- Deployment / Docker → `docs/design/deployment.md`
- Security → `docs/design/security.md`

### Dependency Layers (strict downward flow)

```
PAGES/VIEWS    → Entry points (web pages, CLI commands)
COMPONENTS     → Reusable UI components
API ROUTES     → HTTP handlers
SERVICES       → Business logic
CORE           → Shared libraries (AI, storage, messaging)
INFRASTRUCTURE → Auth, DB, encryption, telemetry, config
TYPES          → Shared type definitions (importable from any layer)
```

Rules: Higher layers may import lower layers. Never the reverse. Components never import from API routes (call via HTTP).

### Core Architecture Patterns

**Factory Pattern**: All external service connections go through factory functions. Never instantiate SDK clients directly. Factories handle configuration, credential resolution, rate limiting, and usage tracking.

**Gateway Pattern**: Auth + permissions + audit composed into a single gateway facade. All API routes use the singleton gateway. Never call auth providers directly outside the gateway.

**Plugin Registry**: Extensible types (notifications, processors, integrations) register via a registry. Core code never checks for specific types.

**Central Config**: One module reads all env vars and exports typed configuration. No scattered `process.env` / `os.environ` reads.

### Observability (Non-Negotiable)

No `console.log/warn/error` (or language equivalent) in production code. Use structured logging:
- Central logging functions with severity levels
- Correlation IDs for request tracing
- Metrics for all external API calls
- Alerting at P0/P1/P2 severity levels

### Key Technologies

| Layer | Technology |
|-------|-----------|
| Framework | cli-tool |
| Language | see brief |
| Database | n/a |
| Auth | n/a |
| Deployment | Docker |
| Observability | Grafana (preferred) or Datadog |

## Formatters

Per-language format & lint normalization is configured in **`.claude/harness-format.toml`** (one `[[formatter]]` row per language: `match` glob, `format_cmd`, optional `lint_cmd`, `check_cmd` for CI, `pin_file`/`pin_field`). This is the human-editable source of truth.

- **Scoped, never whole-repo**: `/mindcoachlabs:verify` formats only the files the current plan touched — never the `--all` form. A one-time whole-repo cleanup of existing drift is its own `style:` PR.
- **Pin versions**: keep each formatter/toolchain pinned (not floating) so local verify and CI agree.
- **CI is the fence**: verify is format-as-you-go convenience; enforcement lives in CI via each row's `check_cmd`.

See `docs/design/format-normalization.md` for the full design, schema, and migration notes.

## Cross-Repo Workspace

Claude Code can access files outside the current repo using absolute paths. For projects with cross-repo dependencies:

**Pattern 1 — Single Session**: Reference both repos by absolute path in your prompts and plan steps. Example: "Read /path/to/api-repo/src/routes.ts and update the scraper in this repo to match."

**Pattern 2 — Workspace Config**: Configure related repos in `.claude/settings.json`:
```json
{
  "workspace": {
    "relatedRepos": [
      { "path": "/absolute/path/to/other-repo", "description": "REST API backend", "role": "dependency" }
    ]
  }
}
```
The prompt-router hook reads this to provide cross-repo context.

**Pattern 3 — Plan Steps**: Include cross-repo dependencies explicitly in plan steps:
```
Step 3: Update API contract
  File(s): /path/to/api-repo/src/routes/data.ts, src/scraper/client.ts
  Action: Ensure both repos use the same field names for the data payload
```

## Environment Setup

**Secrets management**: Secrets are managed per-environment. Never commit plaintext secrets.

Required environment variables:
```bash
DATABASE_URL=...
# AUTH_SECRET_KEY=...
# API_KEY=...
# ENCRYPTION_KEY=...    # Required in production
```

Environment detection is centralized. Never hardcode environment checks.

## Important Conventions

### Pre-Commit Checklist
1. Tests pass
2. Type checker clean
3. Linter clean
4. Build succeeds
5. Design docs updated (if architecture changed)

### Security (Non-Negotiable)
- All user input validated at system boundaries
- All sensitive fields encrypted at rest
- All secrets managed per-environment (dev/staging/prod)
- All endpoints require authentication unless explicitly public
- OWASP Top 10 reviewed for all new endpoints
