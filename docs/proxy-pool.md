# Proxy Pool Architecture

## Two-Tier Model

opencode2claude uses a two-tier proxy pool:

1. **Primary Managed Proxy Pool** (ports 40001-40003)
   - Managed by opencode2claude CLI
   - Can be started, restarted, recovered, rotated
   - Used as main routing targets
   - Docker containers managed via CLI

2. **Warm-Standby Protected Proxy Pool** (ports 40004-40005)
   - Protected anchor proxies
   - Never stopped, restarted, purged, or recreated by CLI
   - Health-checked read-only only
   - Used as fallback when primary fails

## Configuration

```
BRIDGE_PRIMARY_PROXIES=socks5://127.0.0.1:40001,socks5://127.0.0.1:40002,socks5://127.0.0.1:40003
BRIDGE_WARM_STANDBY_PROXIES=socks5://127.0.0.1:40004,socks5://127.0.0.1:40005
BRIDGE_PROXY_POLICY=primary-with-warm-standby
```

## Routing
- Deterministic hashing maps each agent to a stable primary proxy
- If primary fails, request fails over to warm-standby
- Deprecated static port 40010 is removed

## Safety
Ports 40004-40005 are protected infrastructure — `is_protected_proxy_port()` guards all destructive Docker operations.
