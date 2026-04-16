#!/usr/bin/env bash
# Lumina Constellation Installer (DT.11)
# https://github.com/moosenet-io/lumina-constellation
#
# Usage:
#   bash install.sh                     # interactive
#   bash install.sh --preset strix_halo_128  # skip hardware detection
#   bash install.sh --dry-run           # show what would happen
#   bash install.sh --yes               # non-interactive (use detected defaults)

set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEPLOY_DIR="$REPO_DIR/deploy"
DRY_RUN=false
YES_MODE=false
PRESET=""
SKIP_PULL=false

# ── Colors ────────────────────────────────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'
CYAN='\033[0;36m'; BOLD='\033[1m'; NC='\033[0m'

info()    { echo -e "${CYAN}  ▶${NC} $*"; }
success() { echo -e "${GREEN}  ✓${NC} $*"; }
warn()    { echo -e "${YELLOW}  !${NC} $*"; }
error()   { echo -e "${RED}  ✗${NC} $*"; exit 1; }
step()    { echo -e "\n${BOLD}$*${NC}"; }

# ── Arg parsing ───────────────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
    case $1 in
        --dry-run)    DRY_RUN=true ;;
        --yes|-y)     YES_MODE=true ;;
        --preset)     PRESET="$2"; shift ;;
        --skip-pull)  SKIP_PULL=true ;;
        -h|--help)
            echo "Usage: install.sh [--dry-run] [--yes] [--preset PRESET] [--skip-pull]"
            exit 0 ;;
        *) warn "Unknown arg: $1" ;;
    esac
    shift
done

run() {
    if $DRY_RUN; then
        echo -e "    ${YELLOW}[dry-run]${NC} $*"
    else
        "$@"
    fi
}

# ── Banner ────────────────────────────────────────────────────────────────────
echo ""
echo -e "${BOLD}✦ Lumina Constellation Installer${NC}"
echo -e "${CYAN}  25 modules · Local inference · \$0/day${NC}"
echo ""
if $DRY_RUN; then
    echo -e "${YELLOW}  DRY RUN — no changes will be made${NC}\n"
fi

# ── Step 1: Prerequisites ─────────────────────────────────────────────────────
step "Step 1/10: Checking prerequisites"

check_cmd() {
    if command -v "$1" &>/dev/null; then
        success "$1 found ($(command -v "$1"))"
    else
        error "$1 not found. Install it first: $2"
    fi
}

check_cmd docker  "https://docs.docker.com/get-docker/"
check_cmd git     "https://git-scm.com/downloads"
check_cmd curl    "Install curl via your package manager"
check_cmd python3 "https://python.org/downloads/"

# Docker is running?
if ! docker info &>/dev/null; then
    error "Docker daemon not running. Start it and retry."
fi
success "Docker daemon responding"

# ── Step 2: Hardware detection ────────────────────────────────────────────────
step "Step 2/10: Hardware detection"

if [[ -n "$PRESET" ]]; then
    info "Using specified preset: $PRESET"
else
    DETECTED_JSON=$(python3 "$DEPLOY_DIR/detect_hardware.py" 2>/dev/null || echo '{}')
    DETECTED_PRESET=$(echo "$DETECTED_JSON" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('fleet_preset','cpu_only'))" 2>/dev/null || echo "cpu_only")
    DETECTED_CHIP=$(echo "$DETECTED_JSON"   | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('chip_model','Unknown')[:50])" 2>/dev/null || echo "Unknown")
    DETECTED_RAM=$(echo "$DETECTED_JSON"    | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('total_ram_gb',0))" 2>/dev/null || echo "0")
    DETECTED_VRAM=$(echo "$DETECTED_JSON"   | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('estimated_vram_gb',0))" 2>/dev/null || echo "0")
    DETECTED_PLATFORM=$(echo "$DETECTED_JSON" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('platform','cpu_only'))" 2>/dev/null || echo "cpu_only")

    info "Chip:     $DETECTED_CHIP"
    info "RAM:      ${DETECTED_RAM}GB"
    info "VRAM:     ${DETECTED_VRAM}GB (estimated)"
    info "Platform: $DETECTED_PLATFORM"
    info "Preset:   $DETECTED_PRESET"

    if $YES_MODE; then
        PRESET="$DETECTED_PRESET"
    else
        echo ""
        read -rp "  Use preset '$DETECTED_PRESET'? [Y/n] " confirm
        if [[ "${confirm,,}" == "n" ]]; then
            read -rp "  Enter preset name: " PRESET
        else
            PRESET="$DETECTED_PRESET"
        fi
    fi
fi

success "Using preset: $PRESET"

# ── Step 3: Ollama check ──────────────────────────────────────────────────────
step "Step 3/10: Checking Ollama"

OS=$(uname -s)
if command -v ollama &>/dev/null; then
    OLLAMA_VERSION=$(ollama --version 2>/dev/null | head -1 || echo "unknown")
    success "Ollama installed: $OLLAMA_VERSION"
    OLLAMA_RUNNING=false
    if curl -sf http://localhost:11434/api/tags &>/dev/null; then
        success "Ollama is running (http://localhost:11434)"
        OLLAMA_RUNNING=true
    else
        warn "Ollama installed but not running. Start it: ollama serve"
    fi
else
    warn "Ollama not found."
    if $YES_MODE; then
        info "Installing Ollama..."
        run curl -fsSL https://ollama.com/install.sh | sh
    else
        read -rp "  Install Ollama now? [Y/n] " confirm
        if [[ "${confirm,,}" != "n" ]]; then
            run curl -fsSL https://ollama.com/install.sh | sh
        else
            warn "Skipping Ollama install. Set OLLAMA_HOST in .env to a remote Ollama."
        fi
    fi
fi

# ── Step 4: Platform auto-configuration ───────────────────────────────────────
step "Step 4/10: Platform configuration"

if [[ "$PRESET" == strix_halo_* ]]; then
    info "Strix Halo detected — configuring AMD GPU memory and Ollama..."
    run python3 "$DEPLOY_DIR/configure_strix_halo.py" --preset "$PRESET"
    warn "Reboot required after kernel params change (will prompt at end)"
elif [[ "$PRESET" == apple_silicon_* ]] && [[ "$OS" == "Darwin" ]]; then
    info "Apple Silicon detected — configuring Ollama launchd..."
    run python3 "$DEPLOY_DIR/configure_macos.py" --preset "$PRESET"
else
    info "Generic platform — no kernel params needed"
fi

# ── Step 5: Generate .env ──────────────────────────────────────────────────────
step "Step 5/10: Generating .env"

ENV_FILE="$DEPLOY_DIR/.env"
if [[ -f "$ENV_FILE" ]] && ! $DRY_RUN; then
    warn ".env already exists at $ENV_FILE"
    if ! $YES_MODE; then
        read -rp "  Overwrite? [y/N] " confirm
        [[ "${confirm,,}" != "y" ]] && { info "Keeping existing .env"; }
    fi
fi

if ! $DRY_RUN; then
    cp "$DEPLOY_DIR/.env.example" "$ENV_FILE"
    # Set OLLAMA_HOST based on OS
    if [[ "$OS" == "Darwin" ]] || docker info 2>/dev/null | grep -q "Desktop"; then
        OLLAMA_HOST="http://host.docker.internal:11434"
    else
        HOST_IP=$(ip route show default 2>/dev/null | grep default | awk '{print $3}' | head -1)
        OLLAMA_HOST="http://${HOST_IP:-172.17.0.1}:11434"
    fi
    sed -i.bak "s|OLLAMA_HOST=.*|OLLAMA_HOST=${OLLAMA_HOST}|" "$ENV_FILE" 2>/dev/null || \
    python3 -c "
import re, pathlib
p = pathlib.Path('$ENV_FILE')
p.write_text(p.read_text().replace('OLLAMA_HOST=http://host.docker.internal:11434', 'OLLAMA_HOST=${OLLAMA_HOST}'))
"
    # Generate random secrets
    POSTGRES_PASS=$(python3 -c "import secrets; print(secrets.token_hex(16))")
    SOMA_SECRET=$(python3 -c "import secrets; print(secrets.token_hex(32))")
    SOMA_JWT=$(python3 -c "import secrets; print(secrets.token_hex(32))")
    LITELLM_KEY=$(python3 -c "import secrets; print('sk-' + secrets.token_hex(24))")

    python3 << ENVEOF
import pathlib, re
env = pathlib.Path("$ENV_FILE").read_text()
env = env.replace("POSTGRES_PASSWORD=change-me-strong-password", "POSTGRES_PASSWORD=$POSTGRES_PASS")
env = env.replace("SOMA_SECRET_KEY=", "SOMA_SECRET_KEY=$SOMA_SECRET")
env = env.replace("SOMA_JWT_SECRET=", "SOMA_JWT_SECRET=$SOMA_JWT")
env = env.replace("LITELLM_MASTER_KEY=", "LITELLM_MASTER_KEY=$LITELLM_KEY")
pathlib.Path("$ENV_FILE").write_text(env)
ENVEOF
    success ".env generated at $ENV_FILE"
    warn "Edit $ENV_FILE to add API keys (OPENROUTER_API_KEY, GITEA_TOKEN, etc.)"
else
    info "[dry-run] Would generate .env from .env.example with random secrets"
fi

# ── Step 6: Generate LiteLLM config ──────────────────────────────────────────
step "Step 6/10: Generating LiteLLM config"

LITELLM_CONFIG="$DEPLOY_DIR/litellm_config.yaml"
run python3 "$DEPLOY_DIR/generate_litellm_config.py" \
    --preset "$PRESET" \
    --output "$LITELLM_CONFIG"
success "LiteLLM config written to $LITELLM_CONFIG"

# ── Step 7: Model pull ─────────────────────────────────────────────────────────
step "Step 7/10: Model pull"

if $SKIP_PULL; then
    warn "Skipping model pull (--skip-pull)"
else
    # Get model list from preset
    MODELS=$(python3 -c "
import yaml, sys
with open('$DEPLOY_DIR/model_presets.yaml') as f:
    presets = yaml.safe_load(f)
preset = presets.get('$PRESET', {})
for m in preset.get('models', []):
    print(m['name'])
" 2>/dev/null)

    if [[ -z "$MODELS" ]]; then
        warn "No models found for preset $PRESET"
    else
        info "Models to pull:"
        echo "$MODELS" | while read -r model; do
            info "  $model"
        done

        if $YES_MODE; then
            PULL="y"
        else
            read -rp "  Pull all models now? This may take 30-90 minutes. [Y/n] " PULL
        fi

        if [[ "${PULL,,}" != "n" ]]; then
            echo "$MODELS" | while read -r model; do
                info "Pulling $model..."
                run ollama pull "$model" || warn "Failed to pull $model — retry manually"
            done
            success "Models pulled"
        else
            warn "Skipping model pull. Run 'ollama pull MODEL_NAME' manually."
        fi
    fi
fi

# ── Step 8: Docker Compose up ─────────────────────────────────────────────────
step "Step 8/10: Starting services"

cd "$DEPLOY_DIR"

if $YES_MODE; then
    START="y"
else
    read -rp "  Start all Lumina services now? [Y/n] " START
fi

if [[ "${START,,}" != "n" ]]; then
    info "Running docker compose up -d..."
    run docker compose up -d
    success "Services started"
else
    warn "Run 'cd deploy && docker compose up -d' when ready"
fi

# ── Step 9: Health check ──────────────────────────────────────────────────────
step "Step 9/10: Health check"

if ! $DRY_RUN && [[ "${START,,}" != "n" ]]; then
    info "Waiting for Soma dashboard to start (up to 60s)..."
    for i in $(seq 1 12); do
        if curl -sf http://localhost:8082/health &>/dev/null; then
            success "Soma dashboard is up"
            break
        fi
        sleep 5
        echo -n "."
    done
    echo ""
else
    info "[dry-run] Would check http://localhost:8082/health"
fi

# ── Step 10: Done ─────────────────────────────────────────────────────────────
step "Step 10/10: Done!"

echo ""
echo -e "${GREEN}${BOLD}  Lumina Constellation is running!${NC}"
echo ""
echo -e "  ${BOLD}Dashboard:${NC}  http://localhost:8082"
echo -e "  ${BOLD}Preset:${NC}     $PRESET"
echo -e "  ${BOLD}Docs:${NC}       https://github.com/moosenet-io/lumina-constellation"
echo ""
echo -e "  First time? Open the dashboard and complete the setup wizard."
echo ""

# Reboot reminder for Strix Halo
if [[ "$PRESET" == strix_halo_* ]]; then
    echo -e "${YELLOW}  ⚠  REBOOT REQUIRED to apply AMD GPU kernel parameters.${NC}"
    echo -e "     After reboot, verify with: sudo rocm-smi --showmeminfo vram"
    echo ""
fi

success "Installation complete"
