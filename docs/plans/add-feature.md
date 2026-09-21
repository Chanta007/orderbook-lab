# Plan: Add a New Feature

## When to Use

Adding a new user-facing feature that spans multiple layers (UI, API, business logic, database).

## Steps

### 1. Read the relevant design doc

Check `CLAUDE.md` source-to-doc mapping. Read the design doc for the domain this feature touches. If no design doc exists, create one (per HARNESS §2.5).

### 2. Design the data model (if needed)

If the feature requires new database tables or columns:
- Add to schema (migration file)
- Regenerate ORM client if applicable
- Update `docs/design/data-model.md`

### 3. Implement the business logic

Create or modify service-layer code. Follow existing patterns:
- Use factory pattern for external service connections
- Use structured logging (no console.log)
- Handle errors with structured context
- Add rate limiting for external API calls

### 4. Create the API endpoint(s)

Follow the standard API pipeline (see `plans/add-api-route.md`):
```
Auth → Rate Limit → Validate → Access Check → Logic → Response
```

### 5. Build the UI (if applicable)

- Server component for data fetching
- Client component for interactivity
- Responsive: works on mobile (375px+), tablet, desktop
- Loading states (skeleton/spinner)
- Error states (user-friendly message + recovery action)
- Dark mode support

### 6. Add tests

Follow `plans/add-test.md`:
- Unit tests for pure business logic (~70%)
- Integration tests for API routes (~20%)
- E2E tests for critical flows (~10%)

### 7. Add observability

- Structured logging on all new operations
- Metrics for any new external API calls
- Error tracking with component and operation context
- Alert definitions for critical failure modes (if applicable)

### 8. Update documentation

- Update or create the relevant `docs/design/*.md` file
- Update `CLAUDE.md` task navigation if this is a new domain
- Update `HARNESS.md` document index if a new design doc was created

## Related Docs

- **[HARNESS.md](../HARNESS.md)** — Code change workflow (§2.4)
- **[CONSTRAINTS.md](../CONSTRAINTS.md)** — All architectural rules
- **[design/core-architecture.md](../design/core-architecture.md)** — Project structure
