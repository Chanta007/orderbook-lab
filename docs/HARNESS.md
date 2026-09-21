# MindCoachLabs Harness

> **Documentation governance, content standards, and product quality framework.**
>
> This is a project-agnostic governance framework. It defines *how* documentation and code quality are managed across any software project. Technology-specific constraints live in `CONSTRAINTS.md`.

Last updated: March 2026

---

## 1. Product Quality Goals

These goals are the **why** behind every architectural decision, constraint, and UX rule in this codebase. Every feature must advance — or at minimum not regress — these qualities. When in doubt about a design decision, trace it back to these goals.

### Immediate Accessibility

Users should be productive from their first interaction with zero training or setup friction. The application meets people where they are. Onboarding is conversational, not form-driven. Defaults are smart. Help is contextual, not manual-driven.

### Time-to-Value

The gap between "I need something" and "I have a useful result" should be as short as possible. Fast responses for quick tasks. Deep processing when depth is needed. No unnecessary steps between the user and value.

### Security & Privacy

This is a public internet-facing application. Data privacy is absolute. Zero cross-user data leakage. Encryption at rest for sensitive fields. Personal data isolation is non-negotiable. All secrets managed per-environment (dev/staging/prod). Assume hostile actors on every endpoint.

### Reliability & Observability

The system must be observable in production without SSH access. Every error is logged with structured context. Every slow path is measurable. Alerts fire before users notice. Dashboards tell the story without code spelunking.

### Clean, Clear Code

Code is written for the next developer, not the compiler. Simple over clever. Explicit over implicit. Small functions. Obvious naming. No dead code. No commented-out blocks. Every file should be understandable in isolation within 30 seconds.

### Scalable & Supportable

Architecture decisions favor horizontal scalability and operational simplicity. Prefer stateless services. Centralize configuration. Use factory patterns for swappable implementations. Design for the 10x growth scenario without over-engineering for the 100x scenario.

### Mobile-First UX

All user-facing surfaces must work on mobile viewports (375px+). Touch targets 44px minimum. No horizontal scroll. Navigation adapts to viewport. Use `dvh` units for full-height layouts. Test on real devices, not just browser resize.

---

## 2. Documentation Governance

### 2.1 Principles

1. **Single Source of Truth** — Each domain has exactly one design doc. No duplication across files.
2. **Constraints are Declarative** — Rules live in `CONSTRAINTS.md`, not scattered across design docs.
3. **Design Docs are Domain-Scoped** — One file = one architectural domain.
4. **Execution Plans are Numbered Checklists** — An agent or developer can follow sequentially.
5. **Stale Content is Deleted** — Not struck through. Historical specs move to `docs/archive/`.
6. **No Orphaned TODOs** — Every TODO must be resolved or explicitly surfaced in a tracking system.
7. **Proactive Cleanup Duty** — Every file touched is left cleaner than found.
8. **Simplification Bias** — Every change should reduce system complexity, not add to it.

### 2.2 Documentation Structure

```
docs/
├── HARNESS.md              ← This file: governance, standards, quality goals
├── CONSTRAINTS.md          ← Architectural rules and coding conventions
├── design/                 ← Domain-specific architecture specs (one per domain)
│   ├── core-architecture.md
│   ├── data-model.md
│   ├── auth-rbac.md
│   ├── api-design.md
│   ├── testing.md
│   ├── observability.md
│   ├── deployment.md
│   ├── security.md
│   ├── ui-ux.md
│   └── ...
├── plans/                  ← Execution plans (step-by-step guides for common tasks)
│   ├── add-api-route.md
│   ├── add-test.md
│   ├── add-feature.md
│   └── ...
├── runbooks/               ← Debugging playbooks for known failure patterns
│   ├── database-issues.md
│   ├── auth-failures.md
│   └── ...
└── archive/                ← Historical documents (moved here, not deleted)
```

### 2.3 Design Doc Template

Every design doc follows this structure:

```markdown
# {Domain Name}

> Last updated: {Month Year}

## 1. Purpose
{What this domain does and why it exists — 2-3 sentences max.}

## 2. Key Files
| File | Responsibility |
|------|---------------|
| `path/to/file` | {What it does} |

## 3. Architecture
{How it works: diagrams, flow descriptions, component relationships.}

## 4. Flow
{Step-by-step runtime flow for the primary use case.}

## 5. Configuration
{Settings, environment variables, feature flags.}

## 6. Cross-references
{Links to related design docs, CONSTRAINTS sections, plans.}
```

### 2.4 Code Change Workflow

Every code change — whether by a human or an AI agent — must respect these rules:

1. **Documentation first** — Read the relevant design doc and execution plan before writing code.
2. **Tests and build must pass** — Run the full test suite, type checker, linter, and build.
3. **No raw console output** — Use structured logging (central logging system). No `console.log`, `print()`, or equivalent in production code.
4. **Rate limiting for external APIs** — All external API calls must go through rate limiting. No direct calls without backoff.
5. **Update documentation** — If architecture, data models, or flows changed, update the relevant design doc.
6. **No partial commits** — Code + docs + types + tests + observability must all be consistent in a single commit.
7. **Data isolation validation** — If the change touches auth, permissions, or multi-tenant data, verify isolation.
8. **Mobile validation** — If the change modifies layout, scroll behavior, or input handling, test on mobile viewport.
9. **Observability completeness** — All catch blocks must log with structured context. Error messages must include the actual error value.
10. **Security review** — If the change touches auth, encryption, user input handling, or external API integration, review for OWASP Top 10 vulnerabilities.
11. **Plugin and setup script review** — If the change adds, removes, or renames files that are part of the plugin (`plugins/mindcoachlabs/{skills,hooks,scripts,templates,bin}/`) or scaffolded by `plugins/mindcoachlabs/bin/mindcoachlabs-setup`, verify the setup script handles the new layout in both INSTALL and UPDATE (including v0→v1 migration) modes. Verify `plugins/mindcoachlabs/.claude-plugin/plugin.json` and `plugins/mindcoachlabs/hooks/hooks.json` still resolve all referenced paths (CONSTRAINTS §15.3). Verify all scripts use cross-platform compatible constructs (CONSTRAINTS §15.4).
12. **Plugin version bump** — If the change touches plugin code (`plugins/mindcoachlabs/**` or `.claude-plugin/marketplace.json`), bump the `version` field in `plugins/mindcoachlabs/.claude-plugin/plugin.json` per semver (CONSTRAINTS §15.9). Required so `/plugin update mindcoachlabs` propagates the change — without a bump, Claude Code treats the install as already up-to-date and skips the refresh.
13. **Health-check parity** — If the change modifies plugin structure (skills add/remove/rename, hooks add/remove, scripts add/remove, `bin/` contents, manifest fields, marketplace.json schema, install paths), update the `/mindcoachlabs:health-check` skill's check sections per CONSTRAINTS §15.10. Required so the doctor reports remain accurate; stale checks produce false positives that erode trust.

### 2.5 Maintenance Rules

- **Stale docs**: If you find a doc that contradicts the code, fix the doc or fix the code — don't leave the contradiction.
- **Orphan detection**: If a design doc references files that no longer exist, update or archive the doc.
- **New domains**: If you create code in a new domain that doesn't have a design doc, create one.
- **Archive policy**: Docs that describe removed features go to `docs/archive/` with a date prefix.

---

## 3. Content Standards

### 3.1 Writing Style

- **Active voice**: "The factory creates connections" not "Connections are created by the factory"
- **Present tense**: "The system validates input" not "The system will validate input"
- **Concrete over abstract**: Include file paths, function names, config keys
- **No filler**: Remove "In order to", "It should be noted that", "Basically"

### 3.2 Code Examples

- Must be copy-paste runnable (no `...` placeholders without explanation)
- Include imports
- Show error handling
- Use the project's actual patterns (not theoretical ideals)

### 3.3 Diagrams

- ASCII art for architecture diagrams (renders in any terminal/editor)
- Use consistent box-drawing characters
- Label every arrow

---

## 4. Plan Governance

### 4.1 Plan Lifecycle

```
Draft → Approved → In Progress → Complete → Archived
                                    ↓
                                  Failed → Archived (with failure notes)
```

### 4.2 Active Plan File

The file `docs/plans/.active-plan.md` holds the current work-in-progress plan. Only one active plan at a time. Format:

```markdown
# Active Plan: {Title}

> Status: {draft|approved|in-progress|complete|failed}
> Type: {bug|feature}
> Created: {ISO date}

## Context
{Why this work is needed. What was investigated.}

## Steps

### Step 1: {Title}
- **File(s)**: {paths}
- **Action**: {what to do}
- **Acceptance**: {how to verify}
- **Status**: {pending|done|failed}

### Step 2: ...
```

### 4.3 Plan Archive

Completed plans are archived to `docs/plans/archive/{YYYY-MM-DD}-{slug}.md` with a PR Metadata block prepended. This creates an audit trail linking every commit to its research context.

### 4.4 Strict-Docs Enforcement (opt-in)

By **default** the `PreToolUse` guard (`pre-tool-guard.py`) carves out anything under `docs/`, plus `CLAUDE.md` — so documentation-only work can ship without an active plan. For most projects that is fine. But if your project wants *every* edit (docs and `CLAUDE.md` included) to require an active plan/execution, enable **strict-docs mode**. This is fully opt-in and backward-compatible: existing projects are unaffected unless they turn it on.

When strict-docs is ON, the **only** paths that stay always-allowed (no plan required) are:

- `docs/plans/**` — so the harness can always write `.active-plan.md`, `.active-execution.md`, and `archive/**` (prevents a chicken-and-egg deadlock where you couldn't create the plan that unblocks editing).
- `.claude/**` — project settings, and the built-in kill switch (you can always disable the hook here).
- `.git/**` — git internals.

Everything else — `docs/design/*.md`, `docs/research/*.md`, `README.md`, `CLAUDE.md`, source — then requires an active plan (`/mindcoachlabs:plan`) or quick execution (`/mindcoachlabs:do`). All other safety properties are preserved: only `Write`/`Edit`/`NotebookEdit` are gated, an `in-progress` build or active execution allows edits, and the guard **fails open** (exit 0) on any error or unparseable input.

**Enabling** (env var overrides the committed marker; both fail open to OFF):

1. **Committed marker (recommended — durable across sessions, machines, and worktrees).** Create `.mcl/config.json` at the repo root:
   ```json
   { "enforce_docs": true }
   ```
2. **Environment variable (session/CI override).** Set `MCL_ENFORCE_DOCS` — truthy (`1`/`true`/`yes`/`on`) forces ON, falsey (`0`/`false`/`no`/`off`) forces OFF. Mirrors the `MCL_SKIP_DOC_GATE` / `MCL_SKIP_ARCHIVE_GATE` idiom. Because env vars aren't durable per-project, prefer the committed marker for permanent enforcement and use the env var for a temporary local override.

**Wiring a project-level hook (gotcha).** The plugin's own guard is path-safe (it uses `${CLAUDE_PLUGIN_ROOT}`). But if you wire your *own* `PreToolUse` hook in a project's `.claude/settings.json`, do **not** rely on `$CLAUDE_PROJECT_DIR` alone — it can be empty in background/headless runs and points at the wrong path inside git worktrees. Use a worktree-safe fallback so it resolves in both normal checkouts and worktrees:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Write|Edit|NotebookEdit",
        "hooks": [
          { "type": "command", "command": "python3 \"${CLAUDE_PROJECT_DIR:-$(git rev-parse --show-toplevel)}/.claude/hooks/plan-enforce-guard.py\"" }
        ]
      }
    ]
  }
}
```

---

## 5. HARNESS Workflow

The framework provides three workflow paths depending on task scope:

### 5.1 Lightweight Path — `/mindcoachlabs:do`

For ad-hoc work touching 1-5 files with no architectural changes: quick bugfixes, running code, small modifications.

```
/mindcoachlabs:do <task> → execute → verify-lite → commit → archive
```

Auto-promotes to the full pipeline (Plan→Build→Verify) if the scope grows beyond 5 files or requires architectural changes.

### 5.2 Full Pipeline — Plan → Build → Verify

For non-trivial changes requiring research, planning, and tracked execution:

```
Phase 1: /mindcoachlabs:plan    → Research, gather evidence, create implementation plan
Phase 2: /mindcoachlabs:build   → Write code, execute commands, fix failures (tracked in plan)
Phase 3: /mindcoachlabs:verify  → Audit completeness, run checks, commit
```

Each phase is a hard gate — you cannot skip ahead. This prevents:
- Writing code without understanding the domain (Plan gate)
- Untracked fixes diverging from the plan (Fix tracking with MINOR/MAJOR classification)
- Committing without running checks (Verify gate)
- Partial implementations (Completeness audit)

The build phase integrates execution and fix tracking:
- **MINOR fixes** (same files, <20 lines, no design contradiction) are auto-fixed and logged in the plan.
- **MAJOR fixes** (new files, architectural, contradicts plan) stop build and require `/mindcoachlabs:plan modify:` to update the plan before resuming.

### 5.3 Data Pipeline — `/mindcoachlabs:data`

For data gathering, ETL, and pipeline tasks with two tracks:
- **One-off**: Gather → validate → analyze → report → archive
- **Continuous**: Same phases plus operationalize (scheduling, incremental extraction, design doc + runbook creation)

### 5.4 Smart Routing via Hooks

The framework includes a hooks layer (prompt hooks in `.claude/settings.json`) that automatically detects user intent and project state, then suggests the right skill. Users can type free-text requests and the hook adds routing context.

**Decision tree**: When to use which path?
- **Fix a bug / small change (1-5 files)** → `/mindcoachlabs:do`
- **New feature / refactor / complex bug** → `/mindcoachlabs:plan`
- **Data pipeline / ETL / scraping** → `/mindcoachlabs:data`
- **Fully autonomous** → `/mindcoachlabs:agent`

For full hooks architecture, see `docs/design/hooks-routing.md`.

See `skills/*/` (within the plugin) for the full skill definitions.

---

## 6. Execution Plan Templates

### 6.1 Available Templates

| Plan | When to Use |
|------|-------------|
| `docs/plans/add-api-route.md` | Adding a new HTTP endpoint |
| `docs/plans/add-test.md` | Adding test coverage |
| `docs/plans/add-feature.md` | Adding a new user-facing feature |
| `docs/plans/add-data-model.md` | Modifying the database schema |
| `docs/plans/add-observability.md` | Adding logging, metrics, or alerts |
| `docs/plans/add-integration.md` | Integrating with an external service |

### 6.2 Creating New Templates

New plan templates should be created when a task type recurs more than twice. Follow the existing plan format: "When to Use", numbered "Steps", and "Related Docs".

---

## 7. Document Index

### Core Documents

| Document | Purpose |
|----------|---------|
| `CLAUDE.md` | Quick reference auto-loaded by Claude Code |
| `docs/HARNESS.md` | This file — governance and standards |
| `docs/CONSTRAINTS.md` | Architectural rules and coding conventions |

### Design Documents

| Document | Domain |
|----------|--------|
| `docs/design/core-architecture.md` | Technology stack, project structure, rendering strategy |
| `docs/design/data-model.md` | Database schema, relationships, migrations |
| `docs/design/auth-rbac.md` | Authentication, authorization, role-based access |
| `docs/design/api-design.md` | API conventions, endpoint patterns, error handling |
| `docs/design/testing.md` | Test strategy, framework choices, coverage |
| `docs/design/observability.md` | Logging, metrics, alerting, dashboards |
| `docs/design/deployment.md` | Docker, CI/CD, environment management |
| `docs/design/security.md` | Threat model, encryption, input validation |
| `docs/design/ui-ux.md` | Component library, responsive design, accessibility |
| `docs/design/mindcoachlabs-harness.md` | Skills inventory, gateway, workflow, update guide, worktree usage |
| `docs/design/hooks-routing.md` | Hook events, routing decision tree, auto/agent timing model |

### Execution Plans

| Plan | Task Type |
|------|-----------|
| `docs/plans/add-api-route.md` | New API endpoint |
| `docs/plans/add-test.md` | New test coverage |
| `docs/plans/add-feature.md` | New user-facing feature |
| `docs/plans/add-data-model.md` | Database schema change |
| `docs/plans/add-observability.md` | Logging/metrics/alerts |
| `docs/plans/add-integration.md` | External service integration |

---

## 8. Best Practices (Non-Negotiable Defaults)

These are the opinionated defaults that every project using this framework inherits. They represent battle-tested best practices for building production software.

### 8.1 Architecture Patterns

- **Factory Pattern** — All external service connections (LLM, email, storage, payment) go through factory functions. Never instantiate SDK clients directly. This enables testing, monitoring, and provider swaps.
- **Dependency Injection** — Core services are composed via constructor injection or factory composition. No hidden global state. Every dependency is explicit and swappable.
- **Plugin Registry** — Extensible features (artifact types, processing pipelines, notification channels) use a registry pattern. Core code never checks for specific types.
- **Gateway Pattern** — Cross-cutting concerns (auth, rate limiting, audit logging) are composed into a single gateway facade. API routes import the gateway, not individual providers.

### 8.2 Observability (Non-Negotiable)

- **Structured Logging** — Every log entry includes: timestamp, severity, component, operation, correlation ID, and structured metadata. No free-form string logging.
- **Preferred: Grafana Stack** (Loki for logs, Prometheus for metrics, Grafana for dashboards) — Open-source, self-hostable, cost-effective.
- **Alternative: Datadog** — Managed service with excellent integrations but higher cost. Use if operational simplicity outweighs cost concerns.
- **Metrics**: Request duration histograms, error rate counters, queue depth gauges. Every external API call has latency tracking.
- **Alerting**: P0 (page immediately), P1 (respond within 1 hour), P2 (next business day). Define alert thresholds in infrastructure-as-code, not in dashboards.
- **No SSH debugging** — Assume you cannot SSH into production. Every debugging workflow must work through logs, metrics, and dashboards.

### 8.3 Security (Assume Public Internet)

- **OWASP Top 10** — Every endpoint reviewed for injection, XSS, CSRF, broken auth, sensitive data exposure.
- **Encryption at rest** — AES-256-GCM (or equivalent) for all sensitive fields. Keys rotated on schedule.
- **Encryption in transit** — TLS everywhere. No HTTP endpoints in production.
- **Input validation** — Validate at system boundaries with schema validation (Zod, Pydantic, JSON Schema). Trust internal code.
- **Secret management** — Dev/staging/prod secrets are separate. Never commit secrets. Use encrypted env files (dotenvx, SOPS) or a secrets manager (Vault, AWS Secrets Manager). Secrets are injected at runtime, never baked into images.
- **CORS/CSRF** — Explicit origin allowlists. CSRF tokens for state-changing operations. No wildcard CORS in production.
- **Rate limiting** — All public endpoints rate-limited. All external API calls rate-limited with exponential backoff.
- **Dependency scanning** — Automated vulnerability scanning on every PR. No known critical CVEs in production dependencies.

### 8.4 Deployment (Docker-Preferred)

- **Docker containers** — All services packaged as Docker images. Multi-stage builds for minimal image size. Non-root users inside containers.
- **Docker Compose for local dev** — One command to start the full stack locally. Database, cache, app — all containerized.
- **Environment parity** — Dev, staging, and production use the same Docker image. Only configuration differs.
- **CI/CD pipeline** — Automated: lint → test → type-check → build → deploy. No manual deployment steps.
- **Zero-downtime deploys** — Rolling updates or blue-green. Health checks gate traffic routing.
- **Infrastructure as Code** — Terraform, Pulumi, or CDK for all cloud resources. No click-ops.

### 8.5 Git Workflow (Worktree-Integrated)

- **Branch strategy** — `main` (production), `dev` (integration), feature branches for work-in-progress.
- **Git worktrees** — Use `git worktree` for parallel feature development. Each worktree gets its own `.env` copy and can run independently.
- **Worktree launcher** — A Python script (`launch_worktree.py`, cross-platform Windows/macOS/Linux) that creates a worktree branched off the project's default dev branch (auto-detected: `dev` → `develop` → `main` → `master`, first that exists; overridable with `--base <branch>`), copies env files, and opens the IDE.
- **PRs target dev** — Feature branches merge to `dev`. `dev` merges to `main` for releases.
- **Commit messages** — `{type}: {summary}` format. Types: `feat`, `fix`, `refactor`, `docs`, `test`, `chore`. Include `Co-Authored-By` for AI-assisted commits.

### 8.6 Configuration Management

- **Central config** — One module/file that reads all environment variables and exports typed configuration. No scattered `process.env` / `os.environ` reads.
- **Environment detection** — A single function (`detectEnvironment()`) that returns `development`, `staging`, or `production`. All environment-specific behavior branches from this.
- **Feature flags** — Use environment variables or a feature flag service for gradual rollouts. Never use `if (isDev)` checks in business logic.
- **Validation on startup** — The app validates all required configuration at startup and fails fast with a clear error if anything is missing.

### 8.7 Web UI Standards

- **Clean, clear interface** — Minimal chrome. Content-first. No decorative elements that don't serve function.
- **Component library** — Use an established component library (shadcn/ui, Radix, Material UI) for consistency. Copy-paste ownership model preferred over framework dependency.
- **Responsive by default** — Every component works on mobile (375px), tablet (768px), and desktop (1280px+).
- **Dark mode support** — CSS custom properties or Tailwind dark mode classes. No hardcoded colors.
- **Loading states** — Every async operation shows progress. Skeleton screens for initial loads. Inline spinners for actions.
- **Error states** — Every error has a user-facing message. No raw error codes. Recovery actions where possible.
- **Accessibility** — Semantic HTML. ARIA labels on interactive elements. Keyboard navigation. Color contrast ratios meet WCAG AA.
