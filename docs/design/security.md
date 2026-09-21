# Security

> Last updated: <!-- DATE -->

## 1. Purpose

Defines the threat model, encryption strategy, input validation rules, and security review process. This application is public internet-facing and must resist hostile actors on every endpoint.

## 2. Key Files

| File | Responsibility |
|------|---------------|
| <!-- e.g., `src/lib/encryption.ts` --> | AES-256-GCM encryption for sensitive fields |
| <!-- e.g., `src/lib/auth/gateway.ts` --> | Auth gateway (authentication, authorization) |
| <!-- e.g., `src/middleware.ts` / `proxy.ts` --> | CORS, CSRF, rate limiting middleware |
| <!-- e.g., `next.config.ts` / nginx config --> | Security headers (CSP, HSTS, etc.) |

## 3. Threat Model

### Assumptions

- The application is accessible from the public internet
- Users may submit malicious input
- Third-party dependencies may contain vulnerabilities
- API keys and credentials are high-value targets
- Multi-tenant data must never leak between tenants

### OWASP Top 10 Coverage

| Threat | Mitigation |
|--------|------------|
| **A01: Broken Access Control** | Auth gateway with role checks on every route. Data isolation per tenant. |
| **A02: Cryptographic Failures** | AES-256-GCM for sensitive fields. TLS in transit. No plaintext secrets. |
| **A03: Injection** | Schema validation on all input. Parameterized queries (ORM). Auto-escaping templates. |
| **A04: Insecure Design** | Gateway pattern. Factory pattern. Principle of least privilege. |
| **A05: Security Misconfiguration** | CSP headers. HSTS. Minimal Docker images. No default credentials. |
| **A06: Vulnerable Components** | Automated dependency scanning (Dependabot/Snyk). No critical CVEs. |
| **A07: Auth Failures** | Rate limiting on auth endpoints. MFA support. Session expiry. |
| **A08: Data Integrity Failures** | Input validation. Signed artifacts. Verified deployments. |
| **A09: Logging Failures** | Structured logging on all operations. Audit trail for sensitive actions. |
| **A10: SSRF** | No user-controlled URLs in server-side requests. Allowlist external APIs. |

## 4. Encryption

### At Rest

Sensitive fields encrypted using AES-256-GCM:
- API keys, tokens, credentials stored in database
- PII fields as required by compliance
- Session data containing sensitive information

### Key Management

- Encryption key stored in environment variable (`ENCRYPTION_KEY`)
- 64-character hex string (256-bit key)
- Key derivation via scrypt (or equivalent)
- Key rotation procedure documented and tested
- Separate keys per environment (dev/staging/prod)

### In Transit

- TLS 1.2+ for all connections
- HSTS enabled with long max-age
- Certificate management via platform or Let's Encrypt

## 5. Security Headers

| Header | Value | Purpose |
|--------|-------|---------|
| `Content-Security-Policy` | `default-src 'self'; script-src 'self'; ...` | Prevent XSS, injection |
| `Strict-Transport-Security` | `max-age=63072000; includeSubDomains` | Force HTTPS |
| `X-Frame-Options` | `DENY` | Prevent clickjacking |
| `X-Content-Type-Options` | `nosniff` | Prevent MIME sniffing |
| `Referrer-Policy` | `strict-origin-when-cross-origin` | Limit referrer leakage |
| `Permissions-Policy` | `camera=(), microphone=(), geolocation=()` | Disable unused APIs |

## 6. Input Validation

### Rules

1. All user input validated at system boundaries with schema validation
2. Reject unknown fields (strict mode)
3. String length limits on all text fields
4. Numeric range validation
5. Email/URL format validation
6. File upload: type whitelist, size limit, content validation
7. JSON depth limits to prevent DoS

### Where to Validate

```
User Input → API Boundary (validate) → Business Logic (trust) → Database (ORM handles)
              ↑ Validate HERE                                      ↑ Parameterized queries
```

Never validate in business logic. Trust internal code. Only validate at boundaries.

## 7. CORS & CSRF

### CORS

- Explicit origin allowlist (no wildcards in production)
- Credentials only from allowed origins
- Preflight caching for performance

### CSRF

- CSRF tokens for state-changing operations via web forms
- SameSite cookie attribute
- Origin/Referer header validation
- API routes using Bearer tokens are inherently CSRF-safe

## 8. Rate Limiting

| Endpoint Type | Limit | Window | Purpose |
|---------------|-------|--------|---------|
| Auth (login/register) | 10 req | 1 min | Prevent brute force |
| Public read | 100 req | 1 min | Prevent scraping |
| Authenticated write | 30 req | 1 min | Prevent abuse |
| Heavy compute | 5 req | 1 min | Prevent resource exhaustion |

Rate limit responses include `Retry-After` header and return 429.

## 9. Dependency Security

- **Automated scanning**: Dependabot, Snyk, or Trivy on every PR
- **No critical CVEs**: Block deploys with known critical vulnerabilities
- **Minimal dependencies**: Prefer well-maintained packages. Review before adding.
- **Pin versions**: Lock major versions. Allow minor/patch updates.
- **Supply chain**: Use lock files. Verify checksums.

## 10. Audit Trail

All sensitive operations are logged to an immutable audit trail:

| Event | Data Captured |
|-------|--------------|
| Login/logout | User ID, IP, timestamp, success/failure |
| Permission changes | Actor, target user, old role, new role |
| Data access (admin views) | Actor, resource type, query scope |
| Data modification | Actor, resource ID, fields changed |
| API key creation/rotation | Actor, key prefix, timestamp |

Audit logs are:
- Append-only (no updates or deletes)
- Retained for compliance period (minimum 1 year)
- Queryable by actor, action, time range

## 11. Security Review Checklist

For every PR that touches auth, encryption, user input, or external APIs:

- [ ] Input validation present at API boundary
- [ ] SQL queries parameterized (no string concatenation)
- [ ] HTML output auto-escaped
- [ ] Auth check present on all endpoints
- [ ] Rate limiting configured
- [ ] Sensitive data encrypted at rest
- [ ] No secrets in code or logs
- [ ] CORS/CSRF protections maintained
- [ ] Error messages don't leak internal details

## 12. Cross-references

- **[CONSTRAINTS.md](../CONSTRAINTS.md)** — Security constraints (§6)
- **[design/auth-rbac.md](auth-rbac.md)** — Auth gateway, role-based access
- **[design/deployment.md](deployment.md)** — Secret management, Docker security
