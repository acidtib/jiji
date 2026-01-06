# Network Stack Improvement Plan

**Status**: After implementing cluster-wide garbage collection and deployment cleanup
**Date**: 2026-01-06

## Executive Summary

The current network stack (WireGuard + Corrosion + CoreDNS + Control Loop) is **architecturally sound** but has gaps in production readiness. The core issues are:

1. ✅ **FIXED**: Distributed garbage collection (cluster-wide cleanup now implemented)
2. ✅ **FIXED**: Deployment cleanup (cluster-wide deletion during deployments)
3. ⚠️ **REMAINING**: Observability, error recovery, and operational tooling

---

## Critical Improvements (Implement Next)

### 1. **DNS Cache Optimization**
**Current**: CoreDNS has 30-second cache (line 106 in `src/lib/network/dns.ts`)
**Problem**: Very aggressive for distributed systems; causes DNS churn
**Impact**: High - affects all service discovery

**Solution**:
```diff
# In dns.ts, line 106:
-    cache 30
+    cache 300  # 5 minutes - more reasonable for stable services
```

**Recommendation**:
- **5 minutes** for production services (change 30 → 300)
- Keep aggressive updates via control loop + DNS update scripts
- This reduces DNS noise while maintaining correctness via active health checks

---

### 2. **Corrosion Replication Monitoring**
**Current**: No visibility into replication lag or failures
**Problem**: Can't debug distributed consistency issues
**Impact**: High - affects troubleshooting

**Solution**: Add monitoring to control loop:

```bash
# In control_loop.ts, add new function:
check_corrosion_health() {
  # Only run every 20 iterations (10 minutes)
  if [ $((ITERATION % 20)) -ne 0 ]; then
    return
  fi

  log_info "Checking Corrosion health..."

  # Check if Corrosion is running
  if ! systemctl is-active jiji-corrosion &>/dev/null; then
    log_error "Corrosion is not running!"
    return
  fi

  # Check database connectivity
  local test=$($CORROSION_DIR/corrosion query --config $CORROSION_DIR/config.toml "SELECT COUNT(*) FROM servers;" 2>/dev/null || echo "FAILED")
  if [[ "$test" == "FAILED" ]]; then
    log_error "Corrosion database query failed"
  fi

  # Export metrics (if metrics endpoint exists)
  # Could write to /var/lib/jiji/metrics/corrosion.prom for Prometheus scraping
}
```

---

### 3. **Better Error Surfacing**
**Current**: Control loop errors silently swallowed (line 870 in `setup.ts`)
**Problem**: Users don't know if networking fails
**Impact**: Medium-High - affects reliability

**Solution**: Remove silent error handling:

```diff
# In src/lib/network/setup.ts, line 870:
-      } catch (_error) {
-        log.say("└── Continuing without control loop", 2);
-      }
+      } catch (error) {
+        log.error(`Failed to setup control loop: ${error}`, "network");
+        throw new Error(`Network control loop setup failed on ${host}: ${error}`);
+      }
```

---

## High Priority Improvements

### 4. **Health-Based DNS Filtering**
**Current**: DNS returns all containers with `healthy = 1`
**Problem**: Containers can be "healthy" in DB but actually failing
**Impact**: Medium - can route to broken containers

**Enhancement**: Add multiple health states:
```sql
-- Current:
healthy INTEGER DEFAULT 1  -- Binary: 0 or 1

-- Better:
health_status TEXT DEFAULT 'unknown',  -- 'healthy', 'unhealthy', 'degraded', 'unknown'
last_health_check INTEGER,  -- Timestamp
consecutive_failures INTEGER DEFAULT 0  -- Track failure count
```

Control loop would update these fields based on:
- Container running? (current check)
- Port responding? (new: TCP check on app_port)
- Recent health check timestamp? (detect stale records)

---

### 5. **Structured Logging in Control Loop**
**Current**: Plain text logs
**Problem**: Hard to parse, no structured search
**Impact**: Low-Medium - affects debugging

**Enhancement**:
```bash
# Add JSON logging function:
log_json() {
  local level=$1
  local message=$2
  local data=$3

  echo "{\"timestamp\":\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\",\"level\":\"$level\",\"message\":\"$message\",\"data\":$data,\"server\":\"$SERVER_ID\"}"
}

# Usage:
log_json "info" "Container health changed" "{\"container_id\":\"$container_id\",\"healthy\":$healthy}"
```

---

### 6. **Corrosion Database Indexes**
**Current**: Unknown if indexes exist on common queries
**Problem**: Performance degradation with many containers
**Impact**: Medium - affects scalability

**Check needed**: Inspect Corrosion schema for indexes on:
```sql
-- High-traffic queries:
SELECT ... FROM containers WHERE server_id = ?
SELECT ... FROM containers WHERE healthy = 1
SELECT ... FROM servers WHERE last_seen > ?
```

If missing, indexes dramatically improve query performance.

---

## Medium Priority Improvements

### 7. **Split-Brain Detection**
**Current**: No detection of network partitions
**Problem**: Servers could diverge silently
**Impact**: Low-Medium - rare but catastrophic

**Approach**:
- Control loop tracks "servers I can reach via WireGuard"
- If can't reach >50% of servers for 10 minutes → alert/log
- Don't take automatic action (too risky)

---

### 8. **Metrics Export (Prometheus)**
**Current**: No metrics collection
**Problem**: No dashboards, no alerting
**Impact**: Low-Medium - affects operations

**Quick Win**: Export metrics from control loop:
```bash
# Write to /var/lib/jiji/metrics/jiji.prom
cat > /var/lib/jiji/metrics/jiji.prom << EOF
# HELP jiji_containers_total Total containers in cluster
# TYPE jiji_containers_total gauge
jiji_containers_total{server="$SERVER_ID"} $container_count

# HELP jiji_containers_healthy Healthy containers
# TYPE jiji_containers_healthy gauge
jiji_containers_healthy{server="$SERVER_ID"} $healthy_count

# HELP jiji_peers_up WireGuard peers with recent handshake
# TYPE jiji_peers_up gauge
jiji_peers_up{server="$SERVER_ID"} $peers_up

# HELP jiji_corrosion_last_query_ms Last Corrosion query duration
# TYPE jiji_corrosion_last_query_ms gauge
jiji_corrosion_last_query_ms{server="$SERVER_ID"} $query_ms
EOF
```

Then run node_exporter with `--collector.textfile.directory=/var/lib/jiji/metrics`

---

### 9. **Automatic Endpoint Rotation Testing**
**Current**: Endpoint rotation implemented but untested in production
**Problem**: May not work when needed
**Impact**: Low - failover edge case

**Enhancement**: Periodic "chaos engineering" test:
- Every 24 hours, artificially rotate one peer's endpoint
- Verify handshake re-establishes
- Log success/failure

---

### 10. **DNS Failover Mechanism**
**Current**: If CoreDNS crashes, all DNS breaks
**Problem**: Single point of failure
**Impact**: Low - CoreDNS is very stable

**Enhancement**:
- Keep `/etc/hosts` as backup with core services
- If CoreDNS port 53 unreachable, containers fall back to /etc/hosts
- Control loop writes both CoreDNS hosts file AND /etc/hosts

---

## Shell Scripts vs. Native Implementation

### Current Architecture
- **Control loop**: Bash script (~350 lines)
- **DNS update**: Bash script (~50 lines)
- **IP discovery**: curl calls from bash

### Are Shell Scripts Sufficient?

**✅ YES for now**, because:
1. **Simplicity**: Easy to read, debug, modify
2. **Leverage existing tools**: wg, podman, curl are battle-tested
3. **Not on hot path**: Runs every 30s, not per-request
4. **Unix philosophy**: Small, composable scripts
5. **Production-proven**: Many k8s components use bash (kubelet, kubeadm)

**⚠️ Consider migration if**:
1. Control loop exceeds 500 lines (maintainability threshold)
2. Need complex data structures (JSON parsing in bash is brittle)
3. Want unit testing (hard with bash)
4. Performance becomes critical (bash is slower than compiled)

### Hybrid Approach (Recommended)

**Keep bash for orchestration**, but:

```bash
# Good: Simple orchestration
while true; do
  update_heartbeat
  reconcile_peers
  sleep 30
done

# Good: Leverage existing tools
wg show jiji0 dump

# Bad: Complex JSON parsing in bash
echo "$json" | sed 's/.*"ip":"\([^"]*\)".*/\1/'

# Better: Use jq or dedicated tool
echo "$json" | jq -r '.ip'
```

**Improvements without rewriting**:
1. Add `jq` dependency for JSON parsing
2. Extract complex logic to separate scripts
3. Add integration tests (deploy.yml → deploy → verify)
4. Use `set -euo pipefail` consistently (already done ✓)

---

## Operational Tooling Gaps

### 11. **Admin Commands Needed**

```bash
# Cluster health overview
jiji network status
# Output:
# ✓ 3/3 servers online
# ✓ 12 containers healthy
# ✗ 2 peers down >5min
# ✓ Corrosion synced

# Force garbage collection
jiji network gc --dry-run
jiji network gc --force

# Inspect container across cluster
jiji network inspect <container-id>
# Shows: Which server, health status, DNS records, traffic stats

# DNS debugging
jiji network dns <service-name>
# Shows: All DNS entries, which IPs, health status, last updated

# Corrosion introspection
jiji network db query "SELECT * FROM containers WHERE service='web'"
jiji network db stats  # Show table sizes, indexes, query stats
```

---

## Recommended Implementation Priority

### **Phase 1: Stability** (This PR)
1. ✅ Cluster-wide garbage collection
2. ✅ Deployment cleanup
3. ⏭ DNS cache tuning (5 min)
4. ⏭ Remove silent error handling

**Timeline**: Now
**Effort**: 1-2 hours
**Impact**: High - prevents DNS pollution

### **Phase 2: Observability** (Next PR)
1. Corrosion health checks
2. Structured logging
3. Basic metrics export
4. Admin commands (network status, network gc)

**Timeline**: Next week
**Effort**: 1 day
**Impact**: High - enables debugging

### **Phase 3: Resilience** (Future)
1. Health-based DNS filtering
2. Split-brain detection
3. Automatic testing
4. Database indexes

**Timeline**: 1-2 months
**Effort**: 3-5 days
**Impact**: Medium - production hardening

---

## Specific Shell Script Improvements

### Control Loop Enhancements

**Add at the top** (after ITERATION=0):
```bash
# Error handling
trap 'log_error "Control loop crashed: $?"' ERR

# Cleanup on exit
trap 'log_info "Control loop stopping"; exit 0' TERM INT

# Performance tracking
declare -A iteration_times
```

**Add timing**:
```bash
while true; do
  ITERATION=$((ITERATION + 1))
  iteration_start=$(date +%s)

  # ... existing logic ...

  iteration_end=$(date +%s)
  iteration_duration=$((iteration_end - iteration_start))

  if [ $iteration_duration -gt 15 ]; then
    log_warn "Slow iteration: ${iteration_duration}s"
  fi

  sleep $LOOP_INTERVAL
done
```

---

## Testing Strategy

### Current State
- ❌ No automated tests for control loop
- ❌ No integration tests for network setup
- ❌ Manual testing only

### Recommended
1. **Integration test**: Full deployment → verify DNS → redeploy → verify cleanup
2. **Chaos test**: Kill Corrosion → verify recovery
3. **Load test**: 100 containers → measure control loop performance
4. **Network partition test**: Block WireGuard → verify peer rotation

**Quick win**: Add smoke test
```bash
# tests/network_smoke_test.sh
jiji server init --yes
jiji deploy web --yes
sleep 60  # Wait for DNS propagation
curl http://web.jiji:8080/health
jiji deploy web --yes  # Redeploy
sleep 60
# Verify old container cleaned up
[ $(corrosion query "SELECT COUNT(*) FROM containers WHERE service='web'") -eq 1 ]
```

---

## Metrics to Track

### Control Loop
- Iteration duration (p50, p99)
- Containers checked per iteration
- Corrosion query latency
- Garbage collection deletions per hour

### DNS
- Cache hit rate (if CoreDNS exposes)
- Update frequency
- Queries per second

### Corrosion
- Database size growth rate
- Replication lag (if measurable)
- Query errors per minute

### WireGuard
- Handshake age per peer (max, avg)
- Endpoint rotations per day
- Peer downtime

---

## Conclusion

**The network stack is production-ready with these fixes:**
1. ✅ Cluster-wide garbage collection (prevents DNS pollution)
2. ✅ Deployment cleanup (immediate DNS updates)
3. ⏭ DNS cache tuning (reduce churn)
4. ⏭ Better error handling (surface issues)

**Shell scripts are fine** - the bottleneck is distributed systems consistency, not script performance.

**Next most valuable improvements**:
1. Observability (logs, metrics, admin commands)
2. Corrosion health monitoring
3. Health-based routing

**Long-term, consider**:
- Migrating control loop to Go/Rust if it exceeds 500 LOC
- Using CoreDNS Corrosion plugin instead of hosts file
- Adding proper alerting integration (PagerDuty, Slack)
