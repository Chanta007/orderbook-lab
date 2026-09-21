# Plan: Add a New API Route

## When to Use

Adding a new HTTP endpoint to the API layer.

## Steps

### 1. Create the route file

File: appropriate path per your framework's routing convention.

### 2. Follow the standard pipeline

Every API route MUST follow this structure:

```
1. Auth          → authGateway.requireAuth() or requireRole([...])
2. Rate Limit    → checkRateLimit(key, RATE_LIMITS.x)
3. Validate      → Parse and validate input with schema
4. Access Check  → Verify user can access the requested resource
5. Business Logic → Do the work
6. Response      → Return appropriate status code and body
7. Error Handler → Catch, log with structured logging, return error response
```

### 3. Choose the right auth level

| Function | Use When |
|----------|----------|
| `requireAuth()` | Any authenticated user |
| `requireRole(["ADMIN", "MANAGER"])` | Role-restricted endpoints |
| `requireAdmin()` | Platform admin only |

### 4. Add rate limiting

Use the rate limiting module. If existing categories don't fit, add a new one.

### 5. Add resource-level access check (if applicable)

Use `canAccessResource(user, resourceId)` for any endpoint operating on tenant-scoped data.

### 6. For streaming endpoints (SSE)

Use the SSE factory/helper — never build raw streams. The factory provides:
- Automatic keepalive heartbeat
- Time-to-first-token tracking
- Duration logging with severity
- Clean resource cleanup

### 7. Test

- Verify 401 without auth
- Verify 403 with wrong role
- Verify 400 with missing required fields
- Verify 429 when rate limit exceeded
- Verify correct response with valid request

### 8. Update documentation

- Add endpoint to `docs/design/api-design.md` if it represents a new pattern
- Update `CLAUDE.md` task navigation if appropriate

## Related Docs

- **[CONSTRAINTS.md](../CONSTRAINTS.md)** — API conventions (§4)
- **[design/auth-rbac.md](../design/auth-rbac.md)** — Auth gateway methods
- **[design/security.md](../design/security.md)** — Input validation, rate limiting
