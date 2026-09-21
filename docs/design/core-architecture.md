# Core Architecture

> Last updated: <!-- DATE -->

## 1. Purpose

Describes the foundational technology choices, project structure, rendering strategy, and deployment architecture.

## 2. Key Files

| File | Responsibility |
|------|---------------|
| <!-- e.g., `web/next.config.ts` --> | <!-- e.g., Standalone output, security headers --> |
| <!-- e.g., `Dockerfile` --> | <!-- e.g., Multi-stage production build --> |
| <!-- e.g., `docker-compose.yml` --> | <!-- e.g., Local development environment --> |
| <!-- e.g., `package.json` / `pyproject.toml` / `go.mod` --> | <!-- e.g., Dependencies and scripts --> |

## 3. Technology Stack

| Layer | Technology | Notes |
|-------|-----------|-------|
| **Framework** | <!-- e.g., Next.js 16 (App Router) --> | <!-- e.g., Server Components by default --> |
| **Language** | <!-- e.g., TypeScript (strict mode) --> | <!-- e.g., No `any` types --> |
| **Database** | <!-- e.g., PostgreSQL 16 + pgvector --> | <!-- e.g., Via Prisma ORM singleton --> |
| **ORM / Query** | <!-- e.g., Prisma, SQLAlchemy, GORM --> | |
| **Auth** | <!-- e.g., Clerk, Auth0, custom JWT --> | <!-- e.g., Multi-org, webhooks, RBAC --> |
| **Primary LLM** | <!-- e.g., Anthropic Claude --> | <!-- e.g., All calls via factory --> |
| **Styling** | <!-- e.g., Tailwind CSS + shadcn/ui --> | <!-- e.g., Copy-paste owned components --> |
| **Deployment** | Docker | Multi-stage build, standalone output |
| **Encryption** | AES-256-GCM | All sensitive fields encrypted at rest |
| **Observability** | <!-- Grafana or Datadog --> | Structured logging, metrics, alerting |

## 4. Project Structure

```
project-root/
├── docker-compose.yml          # Local dev environment
├── docs/                       # Documentation (HARNESS framework)
│   ├── HARNESS.md
│   ├── CONSTRAINTS.md
│   ├── design/                 # Domain-specific architecture specs
│   ├── plans/                  # Execution plans
│   └── runbooks/               # Debugging playbooks
├── src/                        # Application source code
│   ├── app/                    # Entry points (routes, pages, commands)
│   ├── components/             # Reusable UI components (if applicable)
│   ├── lib/                    # Core libraries and business logic
│   │   ├── auth/               # Auth gateway and providers
│   │   ├── config/             # Central configuration
│   │   ├── db/                 # Database client and helpers
│   │   └── telemetry/          # Logging, metrics, tracing
│   └── types/                  # Shared type definitions
├── tests/                      # Test suites
├── Dockerfile                  # Multi-stage production build
└── .env.example                # Environment variable template
```

## 5. Rendering Strategy

### Server-Side (Default)
All page-level components render on the server. They can directly access the database, auth context, and environment variables.

### Client-Side (Interactive)
Interactive UI is separated into client components that receive server-fetched data as serialized props:
```
Server Component (data fetch) → serialize → Client Component (interactivity)
```

## 6. Docker Architecture

### Local Development

```
docker-compose.yml
├── db (PostgreSQL / MySQL / etc.)
│   ├── Port: 5432
│   ├── Volume: pgdata (persistent)
│   └── Health check: ready
└── app (application)
    ├── Port: 3000
    ├── Depends on: db (healthy)
    └── Env: .env + .env.local
```

### Production Dockerfile

**Stage 1: Builder** — Install dependencies, compile, run type checks, prune dev dependencies.

**Stage 2: Runner** — Non-root user, minimal runtime, copy only production artifacts with `--chown`.

Key requirements:
- Non-root user (e.g., uid 1001) for security
- `--chown` on all runtime data directories (prevents permission errors)
- Health check endpoint for orchestrator
- Startup validates required environment variables

## 7. Configuration

### Central Config Module

All environment variables are read in one place:
```
src/lib/config/index.ts  (or equivalent)
```

Exports typed configuration. Validated at startup. No scattered env reads.

### Environment Detection

```
detectEnvironment() → "development" | "staging" | "production"
```

All environment-specific behavior branches from this function.

## 8. Cross-references

- **[CONSTRAINTS.md](../CONSTRAINTS.md)** — Dependency layers and component rules
- **[design/security.md](security.md)** — Security headers, CORS/CSRF details
- **[design/auth-rbac.md](auth-rbac.md)** — Auth integration and access control
- **[design/deployment.md](deployment.md)** — Docker, CI/CD, environment management
