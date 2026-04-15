# Spectra Security Model

Spectra uses 5 independent security layers. All must hold simultaneously for a malicious page to cause harm.

## Layer 1 — Container Sandbox

The spectra container runs with strict Docker security:

```yaml
security_opt:
  - no-new-privileges:true
  - seccomp:unconfined      # Chromium requires this; custom profile planned
cap_drop: [ALL]
cap_add: [SYS_ADMIN, NET_ADMIN, NET_RAW, CHOWN, DAC_OVERRIDE, SETUID, SETGID]
read_only: true             # Root filesystem read-only
tmpfs:
  - /tmp:size=1g            # Writes go to in-memory tmpfs
  - /dev/shm:size=512m
mem_limit: 2g
cpus: 2.0
pids_limit: 200
```

No Docker socket mount. No host PID namespace. No privileged mode.

## Layer 2 — Network Isolation (iptables)

The `spectra` container blocks all private/link-local ranges via iptables in the entrypoint — BEFORE Chromium starts. Configured via `LAN_RANGES` env var (defaults to all RFC1918):

```bash
# Blocked (no access from spectra container):
10.0.0.0/8      # Private A (LAN services)
172.16.0.0/12   # Docker bridge networks
192.168.0.0/16  # Private C (your LAN)
169.254.0.0/16  # Link-local
127.0.0.0/8     # Loopback (except container-local)

# Allowed:
80/tcp, 443/tcp # Public internet
DNS (optional: restricted to CoreDNS via COREDNS_IP env var)
```

The `spectra-internal` container has the inverse: LAN allowed, internet blocked.

## Layer 3 — Chromium Sandbox

Multi-process Chromium sandbox is ENABLED (not `--no-sandbox`). Fresh `BrowserContext` per session (no cookie sharing). Flags:

```
--disable-extensions
--disable-sync
--disable-background-networking
--disable-component-update
--disable-features=DnsOverHttps
```

Session limits: max 5 concurrent, 15 min max duration, 20 pages per session.

## Layer 4 — Content Sanitization (10 stages)

Every `extract_text` call runs the full pipeline:

1. Parse with BeautifulSoup html.parser
2. Remove dangerous tags: `script`, `style`, `noscript`, `iframe`, `object`, `embed`
3. Remove hidden elements (`display:none`, `visibility:hidden`, `opacity:0`, `aria-hidden`)
4. Remove HTML comments
5. Remove zero-width characters (U+200B, U+200C, U+200D, U+2060, U+FEFF, U+00AD)
6. Remove `data:` URIs from src/href attributes
7. Extract visible text only
8. Normalize whitespace
9. Truncate to 2000 token budget (~8000 chars)
10. Wrap in `[UNTRUSTED_WEB_CONTENT]` / `[/UNTRUSTED_WEB_CONTENT]` delimiters

For Vigil and Seer: double-extraction. Two sanitize passes must produce identical output; disagreement drops content entirely.

## Layer 5 — Access Control + Audit

Every API call is gated by:
- **Key validation**: must be in consumer table (MY.1–MY.9)
- **Enabled check**: disabled keys → 403
- **Daily budget**: 429 when exhausted (resets midnight PDT)
- **Rate limit**: token bucket per consumer (default 5 req/s)
- **Audit log**: every action logged to `audit.jsonl` with consumer, URL, status, content length, sanitization flags

## Attack Vectors and Defenses

| Attack | Defense |
|--------|---------|
| Prompt injection via hidden div | Layer 4: `display:none` elements removed before text extraction |
| JS SSRF to LAN services | Layer 2: iptables DROP all private ranges |
| Meta-refresh to internal URL | Layer 3: Chromium follows redirect into blocked network, connection fails |
| Data exfiltration via URL params | Layer 2: outbound to private IPs blocked |
| Budget bypass (rapid requests) | Layer 5: token bucket rate limiter + daily budget cap |
| Forged consumer key | Layer 5: key must exist in `spectra_config.yaml` |
| DNS rebinding | Layer 2: DNS to public resolvers + private range blocks prevent resolving to LAN |
| Oversized response | Layer 4: truncation at 2000 tokens |
| Malicious page code execution | Layer 1: container sandbox + resource limits |
| Credential leakage | Layer 4: delimiters mark all content as untrusted; LLM instructed not to act on it |
