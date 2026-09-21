# Plan: Add Observability

## When to Use

Adding logging, metrics, or alerting to a new or existing feature.

## Steps

### 1. Identify what to observe

| Observable | Type | Example |
|-----------|------|---------|
| Request handling | Histogram + Counter | API route duration and error rate |
| External API calls | Histogram + Counter | Third-party API latency and failures |
| Background jobs | Counter + Gauge | Job completion rate and queue depth |
| Business events | Counter | User signups, resource creation |
| Error patterns | Counter | Specific error types by component |

### 2. Add structured logging

Replace any raw console output with structured logging:

```
logger.info("Operation completed", {
  component: "service-name",
  operation: "functionName",
  correlationId: requestId,
  duration_ms: elapsed,
  // ...relevant metadata
});
```

### 3. Add metrics

Register counters, histograms, and gauges:

```
// Request duration histogram
recordDuration("http_request_duration", duration, { route, method, status });

// Error counter
incrementCounter("http_errors_total", { route, status, error_type });

// Active connections gauge
setGauge("active_connections", currentCount);
```

### 4. Add error context to catch blocks

Every catch block must include structured logging with:
- Component name
- Operation name
- The actual error value (not just "An error occurred")
- Relevant context (user ID, resource ID, request params)

### 5. Define alerts (if applicable)

Add alert definitions in infrastructure-as-code:

| Alert | Severity | Condition |
|-------|----------|-----------|
| Error rate > 5% | P1 | `rate(errors) / rate(requests) > 0.05` for 5m |
| Latency P95 > 2s | P2 | `histogram_quantile(0.95, duration) > 2` for 10m |
| External API failure | P1 | `rate(external_errors) > 0` for 5m |

### 6. Create or update dashboard

Add panels to the relevant Grafana/Datadog dashboard:
- Latency percentiles (P50, P95, P99)
- Error rate
- Throughput (requests/second)
- Any domain-specific metrics

### 7. Test observability

- Verify logs appear with correct structure
- Verify metrics are recorded
- Trigger error conditions and verify alerts fire
- Check dashboard renders correctly

### 8. Update documentation

- Update `docs/design/observability.md` with new metrics/alerts
- Update relevant domain design doc if observability patterns changed

## Related Docs

- **[design/observability.md](../design/observability.md)** — Observability architecture
- **[CONSTRAINTS.md](../CONSTRAINTS.md)** — Observability rules (§5)
- **[HARNESS.md](../HARNESS.md)** — Code change workflow (§2.4 rule 9)
