# Observability

> Last updated: <!-- DATE -->

## 1. Purpose

Defines the logging, metrics, alerting, and dashboard strategy. The goal is to make every production issue diagnosable without SSH access — purely through logs, metrics, and dashboards.

## 2. Key Files

| File | Responsibility |
|------|---------------|
| <!-- e.g., `src/lib/telemetry/logger.ts` --> | Structured logging functions |
| <!-- e.g., `src/lib/telemetry/metrics.ts` --> | Metrics (counters, histograms, gauges) |
| <!-- e.g., `src/lib/telemetry/spans.ts` --> | Tracing / span management |
| <!-- e.g., `terraform/alerts.tf` --> | Alert definitions (IaC) |

## 3. Architecture

### Observability Stack

**Preferred: Grafana Stack** (open-source, self-hostable, cost-effective)
```
Application → OpenTelemetry SDK
                ├─ Logs → Loki → Grafana Dashboards
                ├─ Metrics → Prometheus → Grafana Dashboards
                └─ Traces → Tempo → Grafana Dashboards (optional)
```

**Alternative: Datadog** (managed, higher cost, excellent integrations)
```
Application → Datadog Agent/SDK
                ├─ Logs → Datadog Log Explorer
                ├─ Metrics → Datadog Metrics
                └─ APM → Datadog APM
```

### Logging Architecture

```
Business Code → logger.info/warn/error()
                    ↓
              Structured JSON
              {timestamp, severity, component, operation, correlationId, ...metadata}
                    ↓
              Log Aggregator (Loki / Datadog / CloudWatch)
                    ↓
              Dashboard Queries + Alerts
```

## 4. Structured Logging

### Log Levels

| Level | When | Example |
|-------|------|---------|
| `debug` | Detailed diagnostic info (dev only) | Function entry/exit, variable values |
| `info` | Normal operations | Request processed, job completed, cache hit |
| `warn` | Degraded but functional | Retry needed, slow query, fallback activated |
| `error` | Operation failed | Unhandled exception, API failure, data corruption |

### Required Fields

Every log entry MUST include:

| Field | Description |
|-------|-------------|
| `timestamp` | ISO 8601 format |
| `severity` | debug / info / warn / error |
| `component` | Module name (e.g., "auth", "api", "worker") |
| `operation` | Function or endpoint name |
| `correlationId` | Request trace ID |

### Anti-Patterns

```
// BAD: Unstructured string
console.log("Processing user " + userId + " at " + new Date());

// BAD: Silent catch
try { await doWork(); } catch (e) { /* swallowed */ }

// GOOD: Structured with context
logger.info("User processed", { component: "auth", userId, duration_ms: 42 });

// GOOD: Error with full context
logger.error("doWork failed", { component: "worker", error: String(e), userId });
```

## 5. Metrics

### Required Metrics

| Metric | Type | Labels |
|--------|------|--------|
| `http_request_duration_seconds` | Histogram | route, method, status |
| `http_request_total` | Counter | route, method, status |
| `external_api_duration_seconds` | Histogram | provider, operation |
| `external_api_errors_total` | Counter | provider, operation, error_type |
| `db_query_duration_seconds` | Histogram | operation, table |
| `active_connections` | Gauge | — |

### Custom Metrics

Add domain-specific metrics for your application's key operations:
- Processing queue depth
- AI/LLM token usage
- Cache hit/miss rates
- Background job completion rates

## 6. Alerting

### Severity Levels

| Severity | Response | Channel | Examples |
|----------|----------|---------|----------|
| **P0** | Page immediately | PagerDuty / SMS | Service down, data loss, security breach |
| **P1** | < 1 hour | Slack + email | Error rate spike, API degradation, auth failures |
| **P2** | Next business day | Email / ticket | Slow queries, disk usage, dependency deprecation |

### Alert Definitions

Alerts are defined in infrastructure-as-code (Terraform, Pulumi), NOT in dashboard UIs:

```hcl
# Example: P1 alert for high error rate
resource "grafana_alert_rule" "high_error_rate" {
  name      = "P1: Error rate > 5%"
  condition = "rate(http_errors_total[5m]) / rate(http_requests_total[5m]) > 0.05"
  for       = "5m"
  labels    = { severity = "P1" }
}
```

### Dashboard Strategy

| Dashboard | Purpose | Audience |
|-----------|---------|----------|
| Service Overview | Health at a glance (error rate, latency, throughput) | On-call engineer |
| API Performance | Per-route latency percentiles and error rates | Backend engineer |
| External APIs | Third-party API health and latency | Integration engineer |
| Database | Query performance, connection pool, disk usage | DBA / backend |

## 7. Production Debugging Workflow

When something breaks in production:

1. **Check the dashboard** — Service Overview for the big picture
2. **Query logs** — Filter by error severity, component, time range
3. **Correlate** — Use correlation ID to trace a single request
4. **Check metrics** — Did latency spike? Error rate change? Queue depth grow?
5. **Check alerts** — Was an alert already firing? What does the runbook say?
6. **Check runbooks** — `docs/runbooks/` for known failure patterns

**Never SSH to diagnose.** If you need SSH, that's a gap in observability.

## 8. Cross-references

- **[CONSTRAINTS.md](../CONSTRAINTS.md)** — Observability rules (§5)
- **[HARNESS.md](../HARNESS.md)** — Code change workflow requiring observability (§2.4)
- **[design/deployment.md](deployment.md)** — How observability integrates with deployment
