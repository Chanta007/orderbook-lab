# Plan: Integrate an External Service

## When to Use

Adding a new third-party API integration (payment provider, email service, storage, AI model, etc.).

## Steps

### 1. Create a factory function

All external service connections go through factory functions. Never instantiate SDK clients directly.

```
// createEmailService({ provider: "sendgrid", apiKey: config.email.apiKey })
// createStorageClient({ provider: "s3", bucket: config.storage.bucket })
```

The factory handles:
- Configuration resolution
- Credential injection
- Rate limiting setup
- Observability hooks

### 2. Add rate limiting

Wrap all external API calls with rate limiting:
- Token bucket algorithm (or equivalent)
- Exponential backoff on 429/503 responses
- Per-provider configuration via environment variables

### 3. Add timeout and retry

| Setting | Default |
|---------|---------|
| Timeout | 15s (60s for LLM) |
| Max retries | 3 |
| Backoff | Exponential (1s, 2s, 4s) with jitter |
| Retry on | 429, 502, 503, 504, network errors |
| Never retry | 400, 401, 403, 404 |

### 4. Add circuit breaker (optional)

For critical integrations, add circuit breaker:
- Open after 5 consecutive failures
- Half-open after 30s cooldown
- Close after successful request

### 5. Add observability

- Latency histogram per provider/operation
- Error counter per provider/error type
- Rate limit hit counter
- Structured logging on all calls (success + failure)

### 6. Manage credentials

- Add API key/credentials to environment variables
- Document in `docs/design/deployment.md` under required env vars
- Separate credentials per environment (dev/staging/prod)
- Add to `.env.example` as placeholder

### 7. Test

- Unit tests with mocked external API
- Verify rate limiting behavior
- Verify timeout/retry behavior
- Verify graceful degradation on API failure

### 8. Update documentation

- Create or update relevant design doc
- Add to `CLAUDE.md` source-to-doc mapping
- Update `docs/design/deployment.md` with new env vars

## Related Docs

- **[CONSTRAINTS.md](../CONSTRAINTS.md)** — External API integration (§13)
- **[design/security.md](../design/security.md)** — Credential management
- **[design/observability.md](../design/observability.md)** — Metrics and logging
