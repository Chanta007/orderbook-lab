# Architectural Constraints

> Last updated: March 2026

This document declares the architectural boundaries, dependency rules, design patterns, and coding conventions that govern this codebase. These constraints are the rules that every code change — whether by a human or an AI agent — must respect.

For documentation governance and content standards, see **[HARNESS.md](HARNESS.md)** §2–3.

---

## 1. Core Technologies

These are the mandatory technologies. Do not introduce alternatives without updating this document.

| Layer | Required Technology | Constraint |
|-------|-------------------|------------|
| Language | <!-- e.g., TypeScript (strict mode), Python 3.12+, Go 1.22+ --> | <!-- e.g., No `any` types in committed code; all exports typed --> |
| Framework | <!-- e.g., Next.js 16, FastAPI, Django, Express --> | <!-- e.g., All pages are Server Components by default --> |
| Database | <!-- e.g., PostgreSQL 16 + pgvector --> | <!-- e.g., All access via ORM singleton --> |
| Auth | <!-- e.g., Clerk, Auth0, Supabase Auth, custom JWT --> | <!-- e.g., Never roll custom auth; identity in auth provider, app data in ORM --> |
| Primary LLM | <!-- e.g., Anthropic Claude, OpenAI GPT-4 --> | <!-- e.g., All calls via factory function --> |
| Styling | <!-- e.g., Tailwind CSS v4 + shadcn/ui --> | <!-- e.g., components/ui/ are owned copy-paste components --> |
| Encryption | AES-256-GCM (or equivalent) | All sensitive fields encrypted at rest; encryption key required in production |
| Deployment | Docker (standalone output) | Multi-stage build; non-root user; production images are minimal |

---

## 2. Design Patterns (Mandatory)

### Factory Pattern

All external service connections MUST go through factory functions. This is non-negotiable.

**Why**: Factories centralize configuration, enable testing with mocks, provide observability hooks, and allow provider swaps without changing business logic.

**Applies to**:
- LLM/AI model connections
- Email sending
- File storage (S3, GCS, local)
- Payment processing
- Any third-party API

**Pattern**:
```
// Good: Factory-mediated
const connection = createLLMConnection({ model: "claude-sonnet-4-5-20250514" });
const result = await connection.complete(prompt);

// Bad: Direct SDK instantiation
const client = new Anthropic({ apiKey: process.env.KEY });
```

### Gateway Pattern

Cross-cutting concerns (auth, rate limiting, audit logging) MUST be composed into gateway facades.

**Pattern**:
```
Gateway
  ├─ IAuthProvider (swappable: Clerk, Auth0, custom)
  ├─ IPermissionStore (swappable: DB, LDAP, policy engine)
  └─ IAuditLogger (swappable: DB, log stream, external service)
```

API routes import the gateway singleton. Never call auth providers directly outside the gateway.

### Plugin Registry

Extensible feature types MUST use a registry pattern. Core code NEVER checks for specific types.

**Pattern**:
```
registerPlugin({ type: "EMAIL", handler: EmailHandler });
registerPlugin({ type: "SLACK", handler: SlackHandler });

// Core code:
const handler = getPlugin(type);  // Never: if (type === "EMAIL") ...
```

### Dependency Injection

Services MUST receive their dependencies through constructor parameters or factory arguments. No hidden global state.

**Pattern**:
```
// Good: Dependencies are explicit
function createService({ db, logger, config }) { ... }

// Bad: Hidden global access
function createService() { const db = getGlobalDB(); ... }
```

---

## 3. Dependency Layers (Strict Downward Flow)

```
PAGES/VIEWS     → Entry points (web pages, CLI commands, API consumers)
COMPONENTS      → Reusable UI components (if applicable)
API ROUTES      → HTTP handlers
SERVICES        → Business logic, domain operations
CORE            → Shared libraries (LLM, storage, messaging)
INFRASTRUCTURE  → Auth, DB, encryption, telemetry, config
TYPES           → Shared type definitions (importable from any layer)
```

**Rules**:
- Higher layers may import from lower layers. Never the reverse.
- Components never import from API routes or services (call via HTTP/RPC).
- Core never imports from services.
- Infrastructure never imports from higher layers.
- Types are importable from any layer.

---

## 4. API Route Conventions

Every API route MUST follow this pipeline:

```
Auth → Rate Limit → Validate Input → Access Check → Business Logic → Response
```

### Standard Error Responses

| Status | Meaning | When |
|--------|---------|------|
| 400 | Bad Request | Input validation failed |
| 401 | Unauthorized | No valid auth token |
| 403 | Forbidden | Authenticated but insufficient permissions |
| 404 | Not Found | Resource doesn't exist |
| 429 | Too Many Requests | Rate limit exceeded |
| 500 | Internal Server Error | Unhandled exception (always log) |

### Input Validation

All input MUST be validated with a schema validation library at the API boundary:
- **TypeScript**: Zod, io-ts, or AJV
- **Python**: Pydantic, marshmallow, or cerberus
- **Go**: go-playground/validator or custom

Never trust user input. Validate shape, types, and ranges. Sanitize strings that will be rendered in HTML.

### Rate Limiting

All public-facing endpoints MUST be rate limited. Configuration:

| Endpoint Type | Default Limit | Window |
|---------------|---------------|--------|
| Read (GET) | 100 req | 1 min |
| Write (POST/PUT/PATCH) | 30 req | 1 min |
| Auth (login/register) | 10 req | 1 min |
| Heavy compute (AI, export) | 5 req | 1 min |

Rate limits are configurable per-environment. Production limits are stricter than development.

---

## 5. Observability (Non-Negotiable)

### 5.1 Logging

**No raw console output in production code.** Use structured logging.

```
// Good: Structured logging
logger.info("Request processed", { component: "api", route: "/users", duration_ms: 42 });

// Bad: Console output
console.log("Request processed in 42ms");
```

Every log entry MUST include:
- **Timestamp** (ISO 8601)
- **Severity** (debug, info, warn, error)
- **Component** (which module/service)
- **Operation** (what function/endpoint)
- **Correlation ID** (trace through request lifecycle)

### 5.2 Metrics

**Preferred: Grafana Stack** (Prometheus + Loki + Grafana)
- Prometheus for metrics collection
- Loki for log aggregation
- Grafana for dashboards and alerting
- Open-source, self-hostable, cost-effective

**Alternative: Datadog**
- Managed APM, logging, and metrics
- Higher cost, lower operational burden
- Better out-of-box integrations

**Required metrics**:
- Request duration (histogram, by route)
- Error rate (counter, by route and status code)
- External API latency (histogram, by provider)
- Queue depth (gauge, if applicable)
- Active connections / sessions (gauge)

### 5.3 Alerting

Define alerts in infrastructure-as-code (Terraform, Pulumi). Never configure alerts only in dashboards.

| Severity | Response Time | Examples |
|----------|---------------|----------|
| P0 | Page immediately | Service down, data loss, security breach |
| P1 | < 1 hour | Error rate spike, API degradation, auth failures |
| P2 | Next business day | Slow queries, disk usage warning, dependency deprecation |

### 5.4 Error Handling

All catch blocks MUST include structured logging. No silent catches.

```
// Good
try {
  await doWork();
} catch (error) {
  logger.error("doWork failed", { error: String(error), component: "worker" });
  throw error;
}

// Bad: Silent catch
try {
  await doWork();
} catch (error) {
  // swallowed
}
```

**Allowed exceptions** (may be silent):
- Cleanup operations (closing connections, removing temp files)
- Optional operations where failure is expected and handled

---

## 6. Security Constraints

### 6.1 Authentication & Authorization

- All endpoints require authentication unless explicitly marked as public.
- Authorization is checked at the API route level, not in business logic.
- Use the auth gateway pattern — never call auth providers directly.
- Session tokens / JWTs have reasonable expiration (e.g., 1 hour access, 30 day refresh).
- Multi-factor authentication supported for sensitive operations.

### 6.2 Data Isolation (Multi-Tenant)

- Every database query that touches user data MUST be scoped to the authenticated user's tenant/organization.
- Personal data views MUST use a deterministic, role-independent query (e.g., `getUserOwnData(userId)`, not `getAllDataForAdmin()`).
- Admin views that show cross-user data MUST be explicitly marked and access-controlled.
- On ambiguity, show nothing — never show the wrong user's data.

### 6.3 Input Security

- All user input is validated at system boundaries with schema validation.
- SQL queries use parameterized statements or ORM (never string concatenation).
- HTML output uses auto-escaping templates (never raw string interpolation for user content).
- File uploads are validated for type, size, and content. No executable uploads.
- JSON parsing has depth limits to prevent DoS.

### 6.4 Secret Management

| Environment | Method |
|-------------|--------|
| Development | `.env.local` (gitignored) or encrypted `.env` (dotenvx/SOPS) |
| Staging | Environment variables in deployment platform or secrets manager |
| Production | Secrets manager (Vault, AWS Secrets Manager, GCP Secret Manager) or encrypted env |

- Never commit plaintext secrets to git.
- Secrets are injected at runtime, never baked into Docker images.
- Rotation procedure documented for every secret type.
- Separate keys per environment (dev/staging/prod). No shared keys.

### 6.5 Dependency Security

- Automated vulnerability scanning on every PR (Dependabot, Snyk, or Trivy).
- No known critical CVEs in production dependencies.
- Pin major versions. Allow minor/patch updates.
- Review new dependencies before adding — prefer well-maintained packages with clear security policies.

---

## 7. Database Conventions

### 7.1 Schema Management

- Schema changes go through migrations (Prisma Migrate, Alembic, Flyway, etc.).
- Never modify production schema manually. All changes via code-reviewed migrations.
- Migrations are idempotent and rollback-safe.
- After schema changes, regenerate ORM client if applicable.

### 7.2 Query Patterns

- Use the ORM for standard CRUD operations.
- Raw SQL only for performance-critical queries or ORM limitations. Always parameterized.
- Eager loading for known N+1 patterns. Lazy loading only when explicitly justified.
- All queries that touch user data include tenant/user scoping.

### 7.3 Indexing

- Every foreign key has an index.
- Every column used in WHERE clauses with high cardinality has an index.
- Composite indexes for multi-column queries.
- Index changes are tested with EXPLAIN ANALYZE before deployment.

---

## 8. Testing Conventions

### 8.1 Testing Pyramid

```
         /   E2E    \          ~10% — Critical user flows only
        /────────────\
       / Integration  \        ~20% — API routes, service interactions
      /────────────────\
     /   Unit Tests     \      ~70% — Pure functions, zero mocking
    /____________________\
```

### 8.2 Test Location

Tests live adjacent to source code in `__tests__/` directories (or equivalent convention for your language).

### 8.3 What to Test vs. Skip

| Test | Skip |
|------|------|
| Pure functions (converters, calculators, parsers) | Third-party UI components |
| Business logic (validation, authorization, processing) | ORM schema (type system validates it) |
| API route handlers (with mocked deps) | Markdown/content files |
| Critical user flows (E2E) | Individual component rendering |

### 8.4 Pre-Commit Checklist

Before committing, all of these must pass:
1. Unit tests
2. Type checker (if applicable)
3. Linter
4. Build

---

## 9. Deployment Conventions

### 9.1 Docker

All services are packaged as Docker images:
- Multi-stage builds: builder stage (full deps) + runner stage (minimal)
- Non-root user inside containers
- Health check endpoints for orchestrator
- Runtime data directories owned by the container user (`--chown`)

### 9.2 Docker Compose (Local Development)

One command to start the full stack:
```bash
docker compose up --build
```

Services include: application, database, cache (if needed), and any dependent services.

### 9.3 Environment Parity

Dev, staging, and production use the **same Docker image**. Only configuration differs:
- Database connection strings
- API keys and secrets
- Feature flags
- Log levels

### 9.4 CI/CD Pipeline

```
Push → Lint → Test → Type Check → Build → Deploy (staging) → Smoke Test → Deploy (prod)
```

- Automated on every push to `dev` (staging) and merge to `main` (production).
- Failed checks block deployment.
- Rollback is automated or one-click.

### 9.5 Merge-Triggered Auto-Deploy: Serialize, Converge, Skip Superseded Work

When a deploy fires automatically on merge to a shared branch (push to `dev`/`main`), it MUST:

1. **Run in a single serialized lane** — `concurrency: { group: deploy-<env>, cancel-in-progress: false }` (keep-newest-pending). Build coalescing with `cancel-in-progress: true` only collapses *temporally overlapping* runs; merges spaced wider than one build never overlap, so N merges → N deploys.
2. **Hold the lane until the rollout CONVERGES** — block the deploy step on real readiness (`aws ecs wait services-stable`, `kubectl rollout status`, `flyctl deploy --wait` — the platform's "wait for steady state" primitive). Never release at the apply / push / task-def-update step while the orchestrator is still rolling.
3. **Skip superseded build work** — at build start, if this commit is already superseded by a newer *image-affecting* commit on the branch, skip the expensive build. Time-window cancellation (`cancel-in-progress`) only collapses *overlapping* builds; a drip spaced wider than one build slips through and wastes CI. A cheap freshness check (`git diff HEAD..origin/<branch>`, treating docs/markdown as non-image-affecting so a docs-only tail never strands the latest code commit) closes the gap.
4. **Gate the APPLY on the LIVE serving commit, not branch HEAD.** A HEAD-based "am I still the latest commit?" apply-gate **starves** under a merge cadence faster than one build+deploy cycle: every run is superseded before its own guard runs, so every run skips and *nothing ever deploys* — the branch stays green while infra changes never apply. Instead, read the **currently-deployed** revision from a repo-defined **live-commit probe** (a health endpoint that echoes the running build, a deployed-image label, a release marker — the repo supplies the exact command) and **APPLY unless doing so would *provably roll the live deployment backwards*** (the built revision is a *strict git ancestor* of the live one, with both SHAs `rev-parse`-normalized before comparing). Three cases all **APPLY**: forward progress (built is ahead of live), `built == live` (a re-apply still converges non-image infra and refreshes the deployed marker), and **probe-unreachable** (FAIL-OPEN — a briefly-down probe must never block convergence). **Skip *only* on a provable rollback.** This is the apply-side complement to rule 3's build-side skip: rule 3 avoids wasted builds, rule 4 guarantees the freshest built artifact actually reaches the live service.
5. **Harden the deploy lane so a stuck run can't wedge it.** Any job that holds the shared deploy concurrency lock MUST set `timeout-minutes` (a cap well below the CI platform's multi-hour default — GitHub Actions defaults to 6h) so a hung run self-clears the lock instead of blocking every following deploy until it is manually cancelled. Any tool in the deploy path that can block waiting on interactive input in CI (e.g. `terraform init`/`apply` prompting for a variable or backend confirmation) MUST run **non-interactively** (`-input=false`, `--yes`, `--non-interactive`, etc.) so a misconfiguration *fails fast* rather than hanging on a prompt no one can answer and holding the lane hostage.

Otherwise a steady stream of merges produces continuous mid-rollout supersession — a "deploy storm" that rotates the live service nonstop and wipes in-flight state. Bound the wait with a timeout and rely on the platform's circuit-breaker / auto-rollback to prevent a wedge.

**Scheduled "floor" deploys follow the same rules.** A scheduled deploy that rebuilds+redeploys the branch tip on a cron (bounding staleness regardless of merge cadence, and covering the case where a fast merge drip cancels every push-triggered build before it starts) MUST run in the **same** `deploy-<env>` concurrency lane and obey rules 2, 4, and 5 identically — so a floor deploy and a push-triggered deploy can never apply concurrently, and the floor never rolls the live service backward. Note that a scheduled workflow's `cron` fires the copy of the workflow on the **default branch** only: floor-workflow fixes take effect only *after* they reach the default branch via a release, so validate scheduled-deploy changes with an on-demand trigger (`workflow_dispatch`) before relying on the cron.

A portable, convergence-safe starting point that implements all five rules lives at `.github/workflows/deploy.yml.tmpl` in the harness templates (bootstrap *proposes* it; it is never auto-installed — repos own their CI).

---

## 10. Mobile UX Constraints

### 10.1 Viewport Rules

- Minimum supported width: 375px (iPhone SE)
- Touch targets: 44px minimum (per Apple HIG)
- No horizontal scroll on any page
- Use `dvh` (dynamic viewport height) for full-height layouts, not `vh`
- Test on actual devices, not just browser resize

### 10.2 Navigation

- Responsive navigation that adapts to viewport:
  - Mobile: hamburger menu or bottom navigation
  - Tablet: collapsible sidebar
  - Desktop: full sidebar or top navigation
- Navigation state persists across page transitions

### 10.3 Forms & Input

- Large touch targets for form inputs
- Appropriate input types (`type="email"`, `type="tel"`, `inputmode="numeric"`)
- Form validation messages visible without scrolling
- Auto-focus on first field after page load

---

## 11. Configuration Conventions

### 11.1 Central Configuration Module

All environment variables are read in ONE place and exported as typed configuration:

```
// config.ts / config.py / config.go
export const config = {
  database: {
    url: requireEnv("DATABASE_URL"),
    poolSize: parseInt(env("DB_POOL_SIZE", "10")),
  },
  auth: {
    secretKey: requireEnv("AUTH_SECRET_KEY"),
    tokenExpiry: parseInt(env("TOKEN_EXPIRY_SECONDS", "3600")),
  },
  // ...
};
```

**Rules**:
- `requireEnv()` throws on missing value with a clear error message.
- `env()` with default for optional values.
- No `process.env` / `os.environ` reads outside this module.
- Validated at startup — fail fast if required config is missing.

### 11.2 Environment Detection

A single function determines the current environment:

```
function detectEnvironment(): "development" | "staging" | "production" {
  // Based on NODE_ENV, APP_ENV, or deployment platform detection
}
```

All environment-specific behavior branches from this function. Never use hostname checks or URL pattern matching.

---

## 12. Git Conventions

### 12.1 Branch Strategy

```
main (production) ← dev (integration) ← feature/* (work in progress)
                                        ← fix/* (bug fixes)
                                        ← chore/* (maintenance)
```

### 12.2 Commit Message Format

```
{type}: {summary under 72 chars}

{Optional body: what and why, not how}

Co-Authored-By: {name} <{email}>
```

Types: `feat`, `fix`, `refactor`, `docs`, `test`, `chore`, `perf`, `style`, `ci`

### 12.3 Worktree Usage

For parallel feature development:
```bash
python launch_worktree.py {name}   # Creates worktree, copies .env, opens IDE
```

Each worktree is independent with its own branch, .env files, and running services.

### 12.4 PR Guidelines

- PRs target `dev`, not `main`
- Include: summary, test results, files changed
- Link to the HARNESS plan if one was used
- Squash merge preferred for clean history

### 12.5 Branch Reconciliation After a History Rewrite

**Symptom:** a `dev`→`main` (integration→release) PR shows as **CONFLICTING despite all-green CI**, AND the branches show a **huge TWO-WAY divergence** (both "ahead by hundreds" of each other) with a **recent merge-base**. This is almost never real divergent work — it is a **SHA artifact of a history rewrite** (an org/remote migration, a rebase, or a force-push changed commit SHAs on one branch), so git sees two lineages that no longer share ancestry even though the *trees are nearly identical*.

**Diagnose before you touch anything** — establish which branch is canonical by content, not by SHA lineage:

```bash
git fetch origin
git diff --name-status <default-branch> <release-branch>   # e.g. dev main
```

If **≈0 files are unique to the release branch** (every difference is a file the active/integration branch also has, newer), then the active branch is a **content superset** — it is canonical, and the release branch is just carrying dead lineage.

**Fix — force-reset the stale branch to the canonical one; NEVER a conflict-merge:**

```bash
git branch backup/<release-branch>-$(date +%Y%m%d) origin/<release-branch>   # 1. BACKUP first
git push origin backup/<release-branch>-...                                   #    (push the backup)
git checkout <release-branch> && git reset --hard <canonical-branch>
git push origin <release-branch> --force-with-lease                           # 2. lease-guarded force
```

- **Never resolve it with a merge** (especially not `-X theirs`): a conflict-merge *permanently records the dead lineage as an ancestor*, so every future release PR re-computes against the garbage merge-base and the problem recurs forever. A reset erases the artifact; a merge enshrines it.
- **This is the ONE sanctioned exception to the never-force-`main` guard** (§12.1, and the tooling's dev→main scope-lock). It qualifies **only** when it is (a) **owner-directed**, (b) preceded by a **pushed backup branch**, and (c) done with **`--force-with-lease`** (never a bare `--force`), after the `git diff --name-status` check has proven the target is a strict subset. Absent all three, the never-force rule stands.
- `/mindcoachlabs:smartdeploy` will **not** perform this reset — its dev→main path is scope-locked to a single `--merge` attempt and bails to a human on CONFLICTING. When it detects this exact signature it prints this recipe as a diagnostic pointer; the operator runs it deliberately.

---

## 13. External API Integration

### 13.1 Rate Limiting

All external API calls MUST go through a rate limiter with:
- Token bucket algorithm (or equivalent)
- Exponential backoff on 429/503 responses
- Per-provider limits configurable via environment

### 13.2 Circuit Breaker

External API calls SHOULD use a circuit breaker pattern:
- Open after N consecutive failures
- Half-open after cooldown period
- Close after successful health check

### 13.3 Timeout Policy

| API Type | Timeout |
|----------|---------|
| Internal microservice | 5s |
| External REST API | 15s |
| LLM/AI completion | 60s |
| File upload/download | 120s |

### 13.4 Retry Policy

- Retry only on transient errors (429, 502, 503, 504, network errors)
- Never retry on 400, 401, 403, 404
- Maximum 3 retries with exponential backoff (1s, 2s, 4s)
- Include jitter to prevent thundering herd

---

## 14. Performance Constraints

### 14.1 Response Time Targets

| Endpoint Type | P50 | P95 | P99 |
|---------------|-----|-----|-----|
| Static page | < 100ms | < 200ms | < 500ms |
| API read | < 200ms | < 500ms | < 1s |
| API write | < 500ms | < 1s | < 2s |
| AI/LLM (streaming) | TTFT < 2s | TTFT < 5s | TTFT < 10s |

### 14.2 Database Query Limits

- No query should take > 1s without an index review
- N+1 queries are a P1 bug
- Connection pool sized for expected concurrency + 20% headroom

---

## 15. Harness Consistency

The `/mindcoachlabs` gateway skill is the primary entry point for all MindCoachLabs work. It must stay in sync with the skills it routes to.

### 15.1 Skill Changes Require Gateway Updates

When adding or modifying a `/mindcoachlabs:X` skill:
- The gateway's **intent classification table** (Step 2) must include the skill with accurate signal words.
- The gateway's **skill catalog** must list the skill with a current description and example.
- The **skills inventory** in `docs/design/mindcoachlabs-harness.md` §2 must include the skill.

### 15.2 Routing Changes Require Documentation Updates

When changing how the gateway routes (intent signals, state overrides, directive prefixes):
- The gateway skill (`plugins/mindcoachlabs/skills/mcl/SKILL.md`) must reflect the change.
- `docs/design/mindcoachlabs-harness.md` §2 and §8 must be updated to match.

### 15.3 Plugin Packaging

The harness is distributed as a Claude Code plugin. Required plugin artifacts:
- `.claude-plugin/marketplace.json` — marketplace catalog at repo root
- `plugins/mindcoachlabs/.claude-plugin/plugin.json` — manifest with `name`, `version`, `skills`, `hooks`
- `plugins/mindcoachlabs/skills/<short>/SKILL.md` — one directory per skill, short name matches the frontmatter `name:` field
- `plugins/mindcoachlabs/hooks/hooks.json` — hook event bindings referencing `python3 ${CLAUDE_PLUGIN_ROOT}/scripts/*.py`
- `plugins/mindcoachlabs/scripts/*.py` — hook implementations (Python 3.8+, stdlib-only)
- `plugins/mindcoachlabs/bin/mindcoachlabs-setup.py` — unified install/update script (Python 3.8+, replaces legacy install.sh + mindcoachlabs_harness_update.sh)
- `templates/` — scaffolded into target projects (CLAUDE.md, docs/*)

When adding or removing files that participate in plugin loading, verify `plugin.json` component paths and `hooks.json` references stay correct. Legacy `.claude/skills/mindcoachlabs-*/` and OpenCode `skill/` directories are NOT part of the plugin and must not be reintroduced.

### 15.4 Cross-Platform Script Compatibility (Python Runtime)

**Hard requirement**: all harness scripts must run identically on Windows, macOS, and Linux. Python 3.8+ is the only runtime dependency — no bash, no POSIX shell tools, no third-party pip packages.

**Covered files**: `plugins/mindcoachlabs/bin/mindcoachlabs-setup.py`, `launch_worktree.py`, `plugins/mindcoachlabs/scripts/*.py`.

**Rules**:
- Shebang: `#!/usr/bin/env python3`. Target Python 3.8+.
- **Stdlib only** — no third-party imports. Acceptable modules: `argparse`, `json`, `os`, `pathlib`, `shutil`, `subprocess`, `sys`, `datetime`, `re`.
- **Paths via `pathlib.Path`** — `Path` handles Windows `\` and POSIX `/` transparently.
- **Subprocess safety** — `subprocess.run([...], shell=False)` with list-form arguments. Never `shell=True`. Never `os.system`.
- **No POSIX-only tools** — no `jq`, `sed`, `awk`, `find`, `grep`, `cp`, `rm`, `chmod` via subprocess. Only `git` and `gh` are permitted external binaries.
- **No platform branches without fallback** — every `if os.name == "nt"` or `sys.platform` check must have a working else branch.
- **Line endings** — write text files with `encoding="utf-8"`.

**Invocation on Windows**: users invoke scripts via `python script.py`. The `hooks.json` command prefix `python3 ${CLAUDE_PLUGIN_ROOT}/scripts/*.py` resolves via PATH when Python 3 is available as `python3` (macOS/Linux) or `python` (Windows).

### 15.5 Do Skill Scope Limits

The `/mindcoachlabs:do` skill is strictly limited to trivially simple changes:
- **≤3 lines of code** in a **single file** (string edits, boolean flips, import fixes, typo corrections, single-value config changes).
- If the change exceeds 3 lines, touches multiple files, or requires architectural reasoning, the work **must** be promoted to `/mindcoachlabs:plan`.
- If the user asks a **follow-up question** or requests additional work after the initial change, the work **must** be promoted to `/mindcoachlabs:plan`.
- All `/do` executions **must** produce an archived plan in the same format as the plan skill (full PR Metadata block + HARNESS Plan table with steps marked `done`).

### 15.6 Harness Enforcement Layers

LLM-driven skill invocation is not reliable on its own — the agent can silently skip skills, drop archive steps, or route around the harness. The harness therefore enforces its rules in three layers, from hardest to softest.

**Layer 1 — Hard enforcement (hooks)**. Hooks run in the harness process and block or surface state regardless of what the LLM does.
- **Claims-slot completeness:** the same `PreToolUse` guard, before the carve-out allow-return, denies Write/Edit/search_replace/write of a claims-shaped JSON object (`document_id` + `claims` list) that fails `validate_document`. Shape-detect, not a path glob. Slot presence, not support/NLI. **Not a third Stop block.** Completeness runs even on `docs/` carve-outs.
- `PreToolUse` (`plugins/mindcoachlabs/scripts/pre-tool-guard.py`): matcher `Write|Edit|NotebookEdit|MultiEdit|search_replace|write|ExitWorktree` (Claude + Grok Build tool names). The source-edit gate covers those mutating edit tools only (any other tool — `Read`, `Bash`, … — exits `0`; accepts Claude `tool_name`/`tool_input` and Grok camelCase `toolName`/`toolInput`). **Ship-merge guard** (`ship-merge-guard.py`, matcher `Bash|run_terminal_command`): blocks `gh pr merge --admin` and `gh pr merge` without `--match-head-commit` unless `MCL_ALLOW_ADMIN_MERGE=1`; **v2.37.0+** also requires review2fix telemetry for PR+head when `--match-head-commit` is present (or `MCL_ALLOW_SKIP_REVIEW=1`). **v2.48.26+ topology:** deny agent `git commit|merge|rebase|cherry-pick|revert|am` (except `--abort`) on named `main`/`master`/`dev` or detached-at those tips; `git -C` and sequential `cd <path> &&`/`;` classify the target (`||`/`|` do not apply cd); empty/non-git no-op; `MCL_ALLOW_ADMIN_MERGE` does not skip topology (`MCL_ALLOW_PROTECTED_COMMIT=1` only). The source-edit gate blocks (`exit 2`) when the target is a source file and neither `docs/plans/.active-plan.md` nor `docs/plans/.active-execution.md` exists. When the file path can't be parsed, falls back to `additionalContext` nudge instead of blocking. A separate branch also gates `ExitWorktree`: it BLOCKS (`exit 2`) `action:"remove"` + `discard_changes:true` when the shared `mcl-deferred` queue is non-empty or `docs/deferred`/`docs/plans` is dirty (fail-open), so a worktree discard can't throw away un-reconciled deferred captures. Carve-outs depend on mode:
  - **Default (strict-docs OFF — backward-compatible behavior for all existing projects):** `docs/`, `.claude/`, `CLAUDE.md`, `.git/`, `.active-plan.md`, `.active-execution.md`.
  - **Strict-docs ON (opt-in):** ONLY `docs/plans/**`, `.claude/**`, `.git/**` stay always-allowed; everything else — including `docs/` and `CLAUDE.md` — then requires an active plan/execution. This closes the gap where docs-only work ships untracked. `docs/plans/**` is kept allowed so the harness can always write `.active-plan.md` / `.active-execution.md` / `archive/**` (no chicken-and-egg deadlock), and `.claude/**` is also the built-in kill switch. **Enable (env overrides committed marker, both fail-open to OFF):** (a) committed marker `.mcl/config.json` → `{"enforce_docs": true}` (durable per-project across sessions/machines/worktrees), or (b) `MCL_ENFORCE_DOCS` env var — truthy (`1/true/yes/on`) forces ON, falsey (`0/false/no/off`) forces OFF (mirrors `MCL_SKIP_DOC_GATE` / `MCL_SKIP_ARCHIVE_GATE`). See HARNESS §4.4.
- `Stop` (`plugins/mindcoachlabs/scripts/stop.py`): at session end, emits state lines for active plan, stale execution, uncommitted changes, untracked-work files, and commits-missing-archive (last 20 commits, filtered for source changes). Warns only — never blocks stop.

**Layer 2 — Soft enforcement (gateway drift check)**. The gateway skill runs "Step 0.5 — Drift Check" before intent classification:
- Drift A: dirty tree + no active plan/execution → STOP and route user to `/mindcoachlabs:do` or `/mindcoachlabs:plan`.
- Drift B: recent commit touched source without an archive → warn and offer backfill.
- Explicit `modify:` directive bypasses both checks (the active plan must exist for modify mode).

**Layer 3 — Instruction enforcement (skill text)**. The skill `SKILL.md` files contain MANDATORY markers on the steps that must never be skipped (archive creation, completion-box display). These rely on the LLM following instructions; Layers 1-2 catch failures.

### 15.7 Plugin-Namespaced Skill References

All skill references — whether in internal routing tables, user-facing `Print:` messages, or documentation — MUST use the plugin-namespaced form `/mindcoachlabs:<skill>`. Legacy forms are violations:
- `/mindcoachlabs-<skill>` (hyphen, pre-plugin)
- `/mindcoachlabs <skill>` (space, pre-plugin gateway-style)

The one exception is historical plan archives under `docs/plans/archive/*.md` — do not rewrite history.

**Covered locations** (must use `/mindcoachlabs:<skill>`):
- Gateway routing tables (intent classification, state overrides, directive prefixes).
- `Print:` messages and quoted user-facing blocks (`> "…"`).
- Next-steps boxes, completion boxes, status boxes.
- Skill catalog entries, including "Ex:" usage examples.
- WARN / error messages.
- Usage lines printed as help when arguments are missing.
- Frontmatter `description:` fields.
- README and design-doc examples.

Rationale: the plugin namespace is the single invocation convention. Legacy forms no longer work once the harness is installed as a plugin. State-override checks (active plan, bootstrap markers, drift) are performed by the gateway skill (`/mindcoachlabs:mcl`) when users invoke it for classification, and by plugin hooks on every prompt.

Enforcement: the verify skill's Step 2 checks this rule on any modified skill file.

### 15.8 Protected Branch Merge Policy

Direct merges into **protected branches** via `/mindcoachlabs:ship` are blocked by default. Changes must land via `/mindcoachlabs:pr`.

- **Default protected-branch set**: `main`, `master`, `production`, `prod`, `release`.
- **Override**: a project may redefine the set by adding a line `Protected branches: <comma-separated list>` to `CLAUDE.md`. An empty list disables protection (use only when the project explicitly opts out).
- **Bypass**: the block may only be skipped when the user passes `--approve-main-merge` to ship. The agent MUST NOT synthesize this flag on the user's behalf — it must come from the user's literal invocation.

The guard fires in two places inside the ship skill:
1. **Parent-branch flow** (Step 2): if the current branch is protected, any local commits that did NOT arrive via a PR merge (`git log … --grep='Merge pull request' --invert-grep`) trigger the block.
2. **Feature-branch flow** (Step 3.3): before checkout/merge, if the resolved `<parent>` is protected, the block fires.

Rationale: human-in-the-loop checkpoint before changes land in a release-bearing branch; preserves a clean PR-based audit and approval trail. Aligns with §12.4 ("PRs target `dev`, not `main`").

**When adding a carve-out**: update both the PreToolUse hook's `case` block AND this section's carve-out list. The list in this doc is the authoritative record of exempt paths.

### 15.9 Plugin Version Bump

Any change to plugin code MUST bump the `version` field in `plugins/mindcoachlabs/.claude-plugin/plugin.json` per semver. Without a version change, `/plugin update mindcoachlabs` is silently a no-op — Claude Code uses the version field to gate updates, so the fix never reaches users even after the PR merges.

**Triggers** (any of these requires a bump):
- `plugins/mindcoachlabs/skills/**` — adding, removing, or modifying any skill
- `plugins/mindcoachlabs/hooks/**` — hook config or hook scripts behind it
- `plugins/mindcoachlabs/scripts/**` — `router.py`, `pre-tool-guard.py`, `stop.py`
- `plugins/mindcoachlabs/bin/**` — `mindcoachlabs-setup` or any future binaries
- `plugins/mindcoachlabs/templates/**` — scaffolded templates
- `plugins/mindcoachlabs/.claude-plugin/plugin.json` — manifest itself
- `.claude-plugin/marketplace.json` — marketplace catalog

**Semver rules**:
- **Patch (1.0.X)** — bug fixes, doc updates inside the plugin, hook tweaks, internal refactors that don't change behavior.
- **Minor (1.X.0)** — new skills, new hooks, new settings/flags, additive changes that stay backwards-compatible.
- **Major (X.0.0)** — breaking changes: skill renames or removals (existing `/mindcoachlabs:foo` invocations stop working), schema/argument changes, hook signature changes, anything that requires users to update their workflow.

**Failure mode**: if you forget the bump, the PR merges to `main`, your fix is on GitHub, but `/plugin update` reports "already at the latest version" and users keep running the broken version. The only escape valves are uninstall+reinstall or a follow-up PR with the bump.

**Verify enforces this**: the `/mindcoachlabs:verify` skill checks that PRs touching plugin paths also bump `plugin.json` version. Failure surfaces in the §2.4 audit as a missing rule 12 step.

Changes to repo-level files outside the plugin (`CLAUDE.md`, `docs/**`, `launch_worktree.py`, `README.md` — anything not under `plugins/mindcoachlabs/` or `.claude-plugin/marketplace.json`) do **not** require a bump.

### 15.10 Health-Check Parity

The `/mindcoachlabs:health-check` skill (`plugins/mindcoachlabs/skills/health-check/SKILL.md`) reports on what should and shouldn't exist on disk in a target project. Its checks are tied to the plugin's structure — when the plugin changes, the checks must change too, or the doctor produces false positives that erode trust.

**Triggers** (any of these requires a corresponding update to health-check):
- Adding, removing, or renaming a skill (changes the expected list in §2 Plugin Installation)
- Adding or removing a hook (changes §2's `hooks/hooks.json` expectations)
- Adding or removing a hook script (changes §2's `scripts/*.py` list)
- Adding or removing a binary in `bin/` (changes §2's `bin/` expectations)
- Changing what `bin/mindcoachlabs-setup` scaffolds into target projects (changes §1 File Inventory)
- Restructuring plugin paths (e.g., the PR #16 subdirectory move) — every check that hard-codes a path must be revisited
- Manifest field changes in `plugin.json` (skills/hooks fields, version-pinning behavior, etc.)
- Marketplace.json schema changes that affect install behavior

**Sections to revisit when triggered**:
- §1 Target-Project File Inventory — files scaffolded by setup script
- §2 Plugin Installation — skill list, hook list, script list, bin list
- §3 Settings Hygiene — what should/shouldn't be in project-local `.claude/settings.json`
- §4 Worktree Health — what counts as a v0 leftover in worktrees
- §8 Legacy Cleanup Scan — what counts as a v0 leftover in target projects

**Failure mode**: if checks aren't updated, health-check either misses real problems (false negatives, harness silently broken) or flags valid v1 setups as broken (false positives, users distrust the doctor and stop running it). Both outcomes defeat the skill's purpose.

**Verify enforces this**: the `/mindcoachlabs:verify` skill checks that PRs touching plugin structure also touch the health-check skill. Failure surfaces in the §2.4 audit as a missing rule 13 step.

This rule pairs with §15.3 (plugin packaging) and §15.9 (version bump): any change to plugin structure cascades into all three artifacts — manifest version, setup script, and health-check expectations.

### 15.11 Stable Plugin Cache Directory (`current/`)

Claude Code's plugin cache stores each version in a separate directory (e.g., `~/.claude/plugins/cache/mindcoachlabs-harness/mindcoachlabs/2.0.9/`). The `${CLAUDE_PLUGIN_ROOT}` variable resolves at session start and doesn't update until restart, causing stale references after `/plugin update`.

The `current/` directory convention solves this:

- **`current/`** — a stable directory maintained by `mindcoachlabs-setup` as a sibling of versioned dirs. Contains a full copy of the active plugin version. All user-facing paths (statusLine commands, manual script invocations) reference `.../current/...` instead of versioned dirs or `${CLAUDE_PLUGIN_ROOT}`.
- **`backup_YYYYMMDDHHMMSS/`** — previous `current/` contents, created automatically before overwrite. Max 2 kept; older backups are pruned.
- **Versioned dirs (`X.Y.Z/`)** — managed by Claude Code's `/plugin` system. Never referenced directly in user-facing config.

**Rules:**
- No symlinks — use `shutil.copytree` for cross-platform compatibility (§15.4).
- `mindcoachlabs-setup` creates/refreshes `current/` in all modes (install, update, migrate).
- Health-check (§15.10) validates `current/` exists and matches the active version.
- Orphan-directory checks exclude `current/` and `backup_*` from cleanup warnings.
- **statusLine config is user-scope only** — write to `~/.claude/settings.json`, never to project-scope `.claude/settings.json`. Claude Code does not expand `${CLAUDE_PLUGIN_ROOT}` in `settings.json` commands (only in plugin `hooks.json`), so project-scope statusLine silently fails.
