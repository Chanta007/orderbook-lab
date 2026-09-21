# Runbook: Database Issues

## Symptoms

- Application returns 500 errors on database queries
- Slow responses across all endpoints
- Connection timeout errors in logs
- "Too many connections" errors

## Diagnosis

### 1. Check connection pool

Look for connection pool exhaustion in logs:
```
grep "connection pool" logs | tail -20
```

### 2. Check slow queries

Query the database for long-running queries:
```sql
SELECT pid, now() - pg_stat_activity.query_start AS duration, query, state
FROM pg_stat_activity
WHERE (now() - pg_stat_activity.query_start) > interval '5 seconds'
ORDER BY duration DESC;
```

### 3. Check disk usage

```bash
SELECT pg_size_pretty(pg_database_size('your_database'));
```

### 4. Check for locks

```sql
SELECT blocked_locks.pid AS blocked_pid,
       blocking_locks.pid AS blocking_pid,
       blocked_activity.query AS blocked_query
FROM pg_catalog.pg_locks blocked_locks
JOIN pg_catalog.pg_locks blocking_locks ON blocking_locks.locktype = blocked_locks.locktype
JOIN pg_catalog.pg_stat_activity blocked_activity ON blocked_activity.pid = blocked_locks.pid
WHERE NOT blocked_locks.granted;
```

## Resolution

### Connection Pool Exhaustion
- Increase pool size in DATABASE_URL or config
- Check for connection leaks (queries not closing)
- Restart application to reset pool

### Slow Queries
- Run EXPLAIN ANALYZE on the slow query
- Add missing indexes
- Optimize query (reduce JOINs, add WHERE clauses)

### Disk Usage
- Identify large tables: `SELECT relname, pg_size_pretty(pg_total_relation_size(relid)) FROM pg_catalog.pg_statio_user_tables ORDER BY pg_total_relation_size(relid) DESC LIMIT 10;`
- VACUUM ANALYZE on large tables
- Archive old data

## Prevention

- Monitor connection pool usage with metrics
- Set up P2 alert for pool usage > 80%
- Set up P1 alert for query duration P95 > 2s
- Regular VACUUM ANALYZE schedule
