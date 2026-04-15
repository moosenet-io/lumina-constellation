FROM mcr.microsoft.com/playwright/python:v1.52.0-noble

# Security: run as non-root (pwuser is pre-created in base image)
USER root

# Install system deps: Xvfb, x11vnc, image processing
RUN apt-get update -qq && apt-get install -y --no-install-recommends \
    xvfb \
    x11vnc \
    openssl \
    iproute2 \
    iptables \
    procps \
    gosu \
    curl \
    && rm -rf /var/lib/apt/lists/*

# Python deps
COPY requirements.txt /app/requirements.txt
RUN pip install --no-cache-dir -r /app/requirements.txt

# App code
WORKDIR /app
COPY spectra_service.py /app/
COPY spectra_worker.py /app/
COPY spectra_sanitizer.py /app/
COPY spectra_config.yaml /app/

# Vendor rrweb (never loaded from CDN at runtime)
RUN mkdir -p /app/static
COPY static/rrweb.min.js /app/static/
COPY static/rrweb-player.min.js /app/static/
COPY static/rrweb-player.css /app/static/

# Entrypoint (runs as root to set up iptables + certs, then drops to pwuser)
COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

# Data dirs (tmpfs in compose, but declare volumes for clarity)
RUN mkdir -p /data/spectra/recordings /data/spectra/screenshots /data/spectra/thumbnails /certs && \
    chown -R pwuser:pwuser /data /certs /app

EXPOSE 8084

HEALTHCHECK --interval=30s --timeout=10s --start-period=30s --retries=3 \
    CMD curl -f http://localhost:8084/health || exit 1

# Entrypoint sets up iptables + TLS as root, then execs as pwuser
ENTRYPOINT ["/entrypoint.sh"]
