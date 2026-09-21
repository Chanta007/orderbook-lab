# Data Model

> Last updated: <!-- DATE -->

## 1. Purpose

Defines the database schema, entity relationships, migration strategy, and data access patterns. This is the single source of truth for how data is structured and accessed.

## 2. Key Files

| File | Responsibility |
|------|---------------|
| <!-- e.g., `prisma/schema.prisma` / `models.py` / `schema.sql` --> | Database schema definition |
| <!-- e.g., `src/lib/db/client.ts` --> | Database client singleton |
| <!-- e.g., `migrations/` --> | Migration files |

## 3. Architecture

### Entity Relationship Overview

```
┌──────────┐     ┌──────────────┐     ┌──────────────┐
│   User   │────<│ Organization │>────│    Member    │
└──────────┘     └──────────────┘     └──────────────┘
                                            │
                                            │
                                      ┌─────┴─────┐
                                      │  Resource  │
                                      └───────────┘
```

### Core Entities

| Entity | Purpose | Key Fields |
|--------|---------|------------|
| User | Identity and profile | id, email, name, createdAt |
| Organization | Multi-tenant container | id, name, slug, plan |
| Member | User↔Org relationship | id, userId, orgId, role |

### Multi-Tenant Data Scoping

Every query that touches user data MUST be scoped to the authenticated user's organization/tenant:

```
// Good: Scoped query
db.resource.findMany({ where: { orgId: user.orgId } })

// Bad: Unscoped query
db.resource.findMany()
```

## 4. Migration Strategy

### Creating Migrations

```bash
# Create a new migration
# npx prisma migrate dev --name {description}
# alembic revision --autogenerate -m "{description}"
# migrate create -ext sql -dir migrations {description}
```

### Migration Rules

1. Every migration is tested locally before pushing
2. Migrations are idempotent (safe to run twice)
3. Destructive migrations (column drops, table deletes) require explicit approval
4. Data migrations are separate from schema migrations
5. After schema changes, regenerate ORM client if applicable

### Rollback Strategy

- Every migration has a corresponding down/rollback migration
- Rollbacks are tested as part of the migration review process
- Production rollbacks are triggered via CI/CD, not manually

## 5. Indexing Strategy

| Index | Table | Columns | Rationale |
|-------|-------|---------|-----------|
| <!-- e.g., idx_resource_org --> | <!-- Resource --> | <!-- orgId, createdAt --> | <!-- Tenant-scoped queries with time ordering --> |

Rules:
- Every foreign key has an index
- Every column in a WHERE clause with high cardinality has an index
- Composite indexes for multi-column queries (most selective column first)
- Validate with EXPLAIN ANALYZE before deploying

## 6. Data Access Patterns

### Singleton Client

The database client is a singleton — one connection pool per process:

```
// Development: Store on globalThis to survive hot reload
// Production: Standard singleton
```

### Encryption at Rest

Sensitive fields are encrypted using AES-256-GCM (or equivalent):
- API keys, tokens, credentials
- Personally identifiable information (PII) as required by compliance
- Encryption key stored in environment, never in code

## 7. Cross-references

- **[CONSTRAINTS.md](../CONSTRAINTS.md)** — Database conventions (§7)
- **[design/auth-rbac.md](auth-rbac.md)** — Data isolation rules
- **[design/security.md](security.md)** — Encryption details
