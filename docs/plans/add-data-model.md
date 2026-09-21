# Plan: Modify the Database Schema

## When to Use

Adding new tables, columns, or relationships to the database.

## Steps

### 1. Read the data model design doc

Read `docs/design/data-model.md` for current schema understanding and conventions.

### 2. Design the schema change

- Define new entities/columns with types and constraints
- Consider: indexes, foreign keys, nullable vs required, defaults
- For multi-tenant apps: ensure tenant scoping on all user data tables

### 3. Create the migration

```bash
# Prisma: npx prisma migrate dev --name {description}
# Alembic: alembic revision --autogenerate -m "{description}"
# Flyway: Create V{N}__{description}.sql
# Django: python manage.py makemigrations
```

### 4. Verify the migration

- Run the migration locally
- Check that rollback/down migration works
- Verify indexes are created
- Run EXPLAIN ANALYZE on key queries

### 5. Regenerate ORM client (if applicable)

```bash
# Prisma: npx prisma generate
# SQLAlchemy: N/A (automatic)
```

### 6. Update business logic

- Create or update service layer to use new schema
- Ensure all queries touching user data include tenant scoping
- Add structured logging for new operations

### 7. Test

- Unit tests for new data access functions
- Verify migration runs cleanly on empty database
- Verify migration runs cleanly on existing database with data

### 8. Update documentation

- Update `docs/design/data-model.md` with new entities and relationships
- Update entity relationship diagram

## Related Docs

- **[design/data-model.md](../design/data-model.md)** — Current schema and conventions
- **[CONSTRAINTS.md](../CONSTRAINTS.md)** — Database conventions (§7)
