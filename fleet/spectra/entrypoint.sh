#!/bin/bash
set -e

echo "[spectra] Starting entrypoint..."

# ── 1. Network isolation (BA.2) ─────────────────────────────────────────────
echo "[spectra] Configuring iptables network isolation..."

# COREDNS_IP: private DNS resolver. Set via environment (defaults to empty = skip specific rule)
COREDNS_IP="${COREDNS_IP:-}"

# Flush existing rules
iptables -F OUTPUT 2>/dev/null || true
iptables -F INPUT  2>/dev/null || true

# Allow established/related connections
iptables -A OUTPUT -m state --state ESTABLISHED,RELATED -j ACCEPT
iptables -A INPUT  -m state --state ESTABLISHED,RELATED -j ACCEPT

# Allow loopback
iptables -A OUTPUT -o lo -j ACCEPT
iptables -A INPUT  -i lo -j ACCEPT

# Allow DNS to internal resolver if configured
if [ -n "$COREDNS_IP" ]; then
    iptables -A OUTPUT -p udp --dport 53 -d "$COREDNS_IP" -j ACCEPT
    iptables -A OUTPUT -p tcp --dport 53 -d "$COREDNS_IP" -j ACCEPT
fi

# Allow public DNS as fallback (blocked for specific resolvers above)
iptables -A OUTPUT -p udp --dport 53 -j ACCEPT
iptables -A OUTPUT -p tcp --dport 53 -j ACCEPT

# Block all private/link-local ranges (RFC1918 + RFC3927)
# Block RFC1918 private ranges + link-local (configured via LAN_RANGES env or defaults)
LAN_RANGES="${LAN_RANGES:-10.0.0.0/8 172.16.0.0/12 192.168.0.0/16 169.254.0.0/16 127.0.0.0/8}"
for RANGE in $LAN_RANGES; do
    iptables -A OUTPUT -d "$RANGE" -j DROP
done

# Allow public internet HTTP/HTTPS
iptables -A OUTPUT -p tcp --dport 80 -j ACCEPT
iptables -A OUTPUT -p tcp --dport 443 -j ACCEPT

# Drop everything else outbound
iptables -A OUTPUT -j DROP

echo "[spectra] Network isolation configured."

# ── 2. TLS certificate generation (BA.1) ────────────────────────────────────
# Generate in /tmp (always writable tmpfs) then copy to /certs volume.
# Note: cap_drop:ALL means no CHOWN/DAC_OVERRIDE — can't chown /certs.
# We write files and they inherit whatever permissions /certs has.

if [ ! -f /certs/spectra.crt ]; then
    echo "[spectra] Generating TLS certificate..."
    openssl req -x509 -newkey rsa:2048 \
        -keyout /tmp/spectra.key \
        -out /tmp/spectra.crt \
        -days 365 -nodes \
        -subj "/CN=spectra/O=Lumina Constellation/C=US" \
        2>/dev/null
    cp /tmp/spectra.crt /certs/spectra.crt
    cp /tmp/spectra.key /certs/spectra.key
    rm /tmp/spectra.crt /tmp/spectra.key
    echo "[spectra] TLS certificate generated."
else
    echo "[spectra] TLS certificate exists, skipping generation."
fi

# ── 3. Start Xvfb virtual display ────────────────────────────────────────────
echo "[spectra] Starting Xvfb..."
Xvfb :99 -screen 0 1280x800x24 -ac &
XVFB_PID=$!
sleep 1
export DISPLAY=:99
echo "[spectra] Xvfb started (PID $XVFB_PID)"

# ── 4. Start x11vnc for Live View ────────────────────────────────────────────
echo "[spectra] Starting x11vnc..."
x11vnc -display :99 -rfbport 5900 -nopw -shared -forever \
    -ssl /certs/spectra.crt /certs/spectra.key \
    -quiet &
echo "[spectra] x11vnc started."

# ── 5. Switch to non-root and start FastAPI ──────────────────────────────────
echo "[spectra] Starting FastAPI service..."
exec gosu pwuser uvicorn spectra_service:app \
    --host 0.0.0.0 \
    --port 8084 \
    --log-level info
