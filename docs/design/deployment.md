# Deployment

> Last updated: <!-- DATE -->

## 1. Purpose

Defines the deployment architecture: Docker builds, CI/CD pipeline, environment management, and zero-downtime deployment strategy.

## 2. Key Files

| File | Responsibility |
|------|---------------|
| `Dockerfile` | Multi-stage production build |
| `docker-compose.yml` | Local development environment |
| <!-- e.g., `.github/workflows/deploy.yml` --> | CI/CD pipeline |
| <!-- e.g., `terraform/` --> | Infrastructure as Code |

## 3. Architecture

### Environment Strategy

```
┌─────────────┐     ┌─────────────┐     ┌─────────────┐
│ Development  │ ──→ │   Staging   │ ──→ │ Production  │
│ (local)      │     │ (auto-deploy│     │ (manual gate│
│              │     │  from dev)  │     │  from main) │
└─────────────┘     └─────────────┘     └─────────────┘
```

All environments use the **same Docker image**. Only configuration differs.

### Docker Build

**Multi-stage Dockerfile**:

```dockerfile
# Stage 1: Builder
FROM node:20-alpine AS builder   # or python:3.12, golang:1.22
WORKDIR /app
COPY . .
RUN npm ci && npm run build      # Install, compile, test
RUN npm prune --production       # Remove dev dependencies

# Stage 2: Runner
FROM node:20-alpine AS runner
RUN addgroup --system app && adduser --system --ingroup app app
WORKDIR /app
COPY --from=builder --chown=app:app /app/.next/standalone ./
COPY --from=builder --chown=app:app /app/public ./public
USER app
EXPOSE 3000
HEALTHCHECK CMD wget -q --spider http://localhost:3000/api/health || exit 1
CMD ["node", "server.js"]
```

Key requirements:
- **Non-root user** — Security best practice
- **`--chown`** — Prevent permission errors on runtime directories
- **Health check** — Orchestrator can verify container is ready
- **Minimal image** — Only production artifacts, no dev tools

### Local Development

```bash
docker compose up --build
```

The compose file includes:
- Application service (your app)
- Database (PostgreSQL, MySQL, etc.)
- Cache (Redis, if needed)
- Any dependent services

### CI/CD Pipeline

```
Push to dev → Lint → Test → Type Check → Build Image → Deploy to Staging
                                                              ↓
Push to main → Same checks → Deploy to Production (with approval gate)
```

**GitHub Actions example**:
```yaml
name: Deploy
on:
  push:
    branches: [dev, main]
jobs:
  deploy:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: npm ci
      - run: npm test
      - run: npm run lint
      - run: npm run build
      - run: docker build -t app .
      - run: docker push registry/app:${{ github.sha }}
      # Deploy step depends on your hosting platform
```

## 4. Environment Configuration

### Secret Management

| Environment | Method |
|-------------|--------|
| Development | `.env.local` (gitignored) or encrypted `.env` (dotenvx/SOPS) |
| Staging | Deployment platform env vars or secrets manager |
| Production | Secrets manager (Vault, AWS SM, GCP SM) or encrypted env |

**Rules**:
- Never commit plaintext secrets
- Separate keys per environment
- Secrets injected at runtime, never baked into images
- Rotation documented for every secret type

### Required Environment Variables

```bash
# Database
DATABASE_URL=postgresql://...

# Auth
# AUTH_SECRET_KEY=...
# AUTH_PROVIDER_KEY=...

# Encryption
ENCRYPTION_KEY=...    # 64-char hex, required in production

# Observability
# GRAFANA_API_KEY=... or DATADOG_API_KEY=...

# Application
NODE_ENV=production   # or APP_ENV
PORT=3000
```

## 5. Zero-Downtime Deployment

### Strategy: Rolling Update

1. New container starts alongside old container
2. Health check passes on new container
3. Traffic routes to new container
4. Old container receives drain signal
5. Old container shuts down after in-flight requests complete

Under concurrent merge-triggered deploys, the rollout must converge before the deploy lane is released — see CONSTRAINTS §9.5 (serialize + converge + skip-superseded + gate-apply-on-live-commit + hardened-lane). A portable, convergence-safe starting point ships at `templates/.github/workflows/deploy.yml.tmpl` (bootstrap proposes it; never auto-installed).

### Rollback

- One-click rollback in deployment platform
- Or: `docker tag registry/app:{previous-sha} registry/app:latest && deploy`
- Database rollbacks are separate (use migration rollbacks)

## 6. Infrastructure as Code

All cloud resources defined in code (Terraform, Pulumi, CDK):
- Compute (containers, functions)
- Database (provisioning, backups)
- Networking (load balancer, DNS)
- Monitoring (alerts, dashboards)
- IAM (roles, permissions)

No click-ops. Every infrastructure change is code-reviewed and versioned.

## 7. Cross-references

- **[CONSTRAINTS.md](../CONSTRAINTS.md)** — Deployment conventions (§9)
- **[design/core-architecture.md](core-architecture.md)** — Docker architecture
- **[design/observability.md](observability.md)** — Monitoring integration
- **[design/security.md](security.md)** — Secret management details
