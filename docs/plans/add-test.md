# Plan: Add a Test

## When to Use

When adding a new pure function, modifying business logic, or adding a new API endpoint that needs test coverage.

## Steps

### 1. Identify the test layer

| If you are testing... | Use | Location |
|----------------------|-----|----------|
| A pure function (no DB, no HTTP) | Unit test | Adjacent `__tests__/` directory |
| An API route handler | Integration test | Adjacent `__tests__/` directory |
| A user flow in the browser | E2E test | `e2e/` or `tests/e2e/` |

### 2. Create the test file

Place tests adjacent to source code:

```
src/lib/{module}/
├── my-module.ts
└── __tests__/
    └── my-module.test.ts
```

### 3. Write the test

Follow the Arrange / Act / Assert pattern:

```
// Arrange
const input = createTestInput();

// Act
const result = myFunction(input);

// Assert
expect(result).toBe(expectedOutput);
```

### 4. Test the happy path first

Write 1-2 tests for the normal case. Verify they pass:

```bash
# Run the specific test file
# npx vitest run path/to/test.ts
# pytest path/to/test.py
# go test ./path/to/...
```

### 5. Add edge cases

- Empty input / zero values
- Boundary conditions (exactly at limits)
- Error cases (invalid input, missing required fields)
- Null/undefined/None handling

### 6. Run the full suite

All tests must pass before committing.

```bash
# npm test / pytest / go test ./...
```

### 7. Check coverage (optional)

Review that the new function has meaningful line coverage.

## For Integration Tests (API Routes)

Mock external dependencies (database, auth) before importing the route handler. Call the handler directly and assert response status and body.

## Related Docs

- **[design/testing.md](../design/testing.md)** — Testing architecture, framework choices
- **[CONSTRAINTS.md](../CONSTRAINTS.md)** — Testing conventions (§8)
