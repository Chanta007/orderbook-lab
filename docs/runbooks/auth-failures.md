# Runbook: Authentication Failures

## Symptoms

- Users cannot log in
- 401 errors on authenticated endpoints
- 403 errors for users who should have access
- Auth webhook failures in logs

## Diagnosis

### 1. Check auth provider status

Verify your auth provider (Clerk, Auth0, etc.) is operational:
- Check their status page
- Test a direct API call to the auth provider

### 2. Check webhook delivery

If using webhooks for user/org sync:
```
grep "webhook" logs | grep "error" | tail -20
```

Common issues:
- Webhook secret mismatch
- Webhook URL changed after deployment
- Auth provider rate limiting webhook delivery

### 3. Check token validation

Look for token validation errors:
```
grep "token" logs | grep "invalid\|expired\|missing" | tail -20
```

### 4. Check CORS/CSRF

If browser-based auth is failing:
- Verify CORS origin allowlist includes the current domain
- Check CSP headers aren't blocking auth provider scripts
- Verify cookies have correct SameSite and Secure attributes

## Resolution

### Token Issues
- If tokens are expired: check clock sync between servers
- If tokens are invalid: verify signing key hasn't rotated
- If tokens are missing: check middleware order (auth before route handler)

### Webhook Sync Issues
- Re-sync users from auth provider to local database
- Verify webhook secret matches between provider and app config
- Check that webhook endpoint is accessible (not behind auth itself)

### Permission Issues
- Verify user's role in the auth provider matches expected
- Check permission store (database) for correct role mapping
- Verify data isolation queries use correct tenant scoping

## Prevention

- P1 alert on auth failure rate > 5%
- Monitor webhook delivery success rate
- Log all auth events with structured context
- Regular audit of role assignments
