# Testing

> Last updated: <!-- DATE -->

## 1. Purpose

Automated testing ensures that code changes don't break existing functionality, that business logic behaves correctly under edge cases, and that critical user flows work end-to-end. The testing strategy is optimized for highest confidence per engineer-hour: unit tests for pure functions, integration tests for API routes, E2E tests for critical flows.

## 2. Key Files

| File | Responsibility |
|------|---------------|
| <!-- e.g., `vitest.config.ts` / `pytest.ini` / `go.mod` --> | Test framework configuration |
| <!-- e.g., `playwright.config.ts` --> | E2E test configuration (if applicable) |
| <!-- e.g., `src/lib/**/__tests__/` --> | Unit test files |

## 3. Architecture

### Testing Pyramid

```
         /   E2E    \          ~10% — Critical user flows only
        /────────────\
       / Integration  \        ~20% — API routes, service interactions
      /────────────────\
     /   Unit Tests     \      ~70% — Pure functions, zero mocking
    /____________________\
```

### Framework Choices

| Tool | Layer | Why |
|------|-------|-----|
| <!-- e.g., Vitest / pytest / go test --> | Unit + Integration | <!-- e.g., Fast, native TypeScript support --> |
| <!-- e.g., Playwright / Cypress --> | E2E | <!-- e.g., Multi-browser, auto-wait --> |

## 4. Running Tests

```bash
# Unit + Integration
# npm test / pytest / go test ./...

# Watch mode
# npm run test:watch / pytest --watch

# Coverage
# npm run test:coverage / pytest --cov

# E2E
# npm run test:e2e / playwright test
```

## 5. Test File Placement

Tests live adjacent to source code:

```
src/lib/auth/
├── gateway.ts
├── factory.ts
└── __tests__/
    ├── gateway.test.ts
    └── factory.test.ts
```

## 6. What to Test vs. Skip

| Test | Skip |
|------|------|
| Pure functions (converters, calculators, parsers) | Third-party UI components |
| Business logic (validation, authorization, processing) | ORM schema (type system validates it) |
| API route handlers (with mocked dependencies) | Content/markdown files |
| Critical user flows (E2E) | Individual component rendering |

## 7. Writing Tests

### Unit Test Pattern

```
// Arrange
const input = createTestInput();

// Act
const result = myFunction(input);

// Assert
expect(result).toBe(expectedOutput);
```

### Integration Test Pattern

```
// Mock external dependencies
mock(database, "findMany", () => [testRecord]);
mock(authGateway, "requireAuth", () => testUser);

// Call the route handler
const response = await POST(createRequest(body));

// Assert response
expect(response.status).toBe(200);
expect(await response.json()).toEqual(expected);
```

### Edge Cases to Always Cover

- Empty input / zero values
- Boundary conditions (at limits)
- Error cases (invalid input, missing required fields)
- Null/undefined handling
- Concurrent access (if applicable)

## 8. Pre-Commit Checklist

All must pass before committing:
1. `test` — All unit + integration tests
2. Type checker (if applicable)
3. Linter
4. Build

## 9. Cross-references

- **[CONSTRAINTS.md](../CONSTRAINTS.md)** — Testing conventions (§8)
- **[HARNESS.md](../HARNESS.md)** — Code change workflow requiring tests (§2.4)
- **[plans/add-test.md](../plans/add-test.md)** — Step-by-step plan for adding tests
