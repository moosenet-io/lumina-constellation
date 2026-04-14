# Partner Claw Onboarding Runbook

Step-by-step guide for deploying a second IronClaw instance on MooseNet as a household partner agent.

## Prerequisites

- Proxmox cluster with available PVS capacity
- Infisical access for secret management
- Matrix account (existing or new) for the partner agent
- the operator has approved the deployment and chosen a name

## Phase 1: Infrastructure Setup

### 1.1 Create new LXC on PVS

```bash
# On PVS host (<pvs-host-ip>)
# Allocate new CT (e.g. CT316)
pct create 316 /var/lib/vz/template/cache/debian-12-standard.tar.zst \
  --hostname partner-agent \
  --cores 2 --memory 2048 \
  --storage local-lvm --rootfs 20 \
  --net0 name=eth0,bridge=vmbr0,ip=dhcp \
  --onboot 1
pct start 316
```

### 1.2 Install IronClaw

```bash
# Inside CT316
wget -O /usr/local/bin/ironclaw \
  https://github.com/nearai/ironclaw/releases/download/ironclaw-v0.24.0/ironclaw-x86_64-unknown-linux-gnu.tar.gz
# Extract and install (see CLAUDE.md for exact process)
chmod +x /usr/local/bin/ironclaw
```

### 1.3 Connect to Terminus (CT214)

```bash
# CT214 must allow SSH from CT316's IP
# In CT216's stdio.sh:
export LUMINA_AGENT_ID=partner    # or chosen name
export NEARAI_AUTH_URL=http://127.0.0.1
export NEARAI_BASE_URL=http://127.0.0.1

/usr/local/bin/ironclaw run --no-onboard
```

## Phase 2: Identity Configuration

### 2.1 Run the naming ceremony

```bash
python3 /opt/lumina-fleet/naming_ceremony.py
# Choose a constellation name that complements the existing setup
# Default agent names can be kept or customized
```

### 2.2 Create workspace/PARTNER.md

Copy the template from `/opt/lumina-fleet/household/partner_identity.md` to:
```
/root/.ironclaw/workspace/PARTNER.md   # or LUMINA.md for this instance
```

Replace `[PARTNER_NAME]` with the chosen name.

### 2.3 Configure Matrix channel

```bash
# In /root/.ironclaw/.env
SIGNAL_ENABLED=false
# Or configure Matrix:
# MATRIX_HOMESERVER=https://your.matrix.server
# MATRIX_USER=@partner-agent:your.matrix.server
```

## Phase 3: Nexus Integration

### 3.1 Register partner in household config

Edit `/opt/lumina-fleet/nexus/household_config.py`:
```python
HOUSEHOLD_AGENTS = ['lumina', 'partner']  # add chosen agent_id
```

### 3.2 Test household messaging

```bash
# On CT310
python3 /opt/lumina-fleet/nexus/household_routing.py
# Should send test household message and receive it as lumina
```

### 3.3 Verify Nexus access

Partner agent needs read/write to inbox_messages table in CT300 Postgres.
Add credentials to partner CT's environment (use Infisical).

## Phase 4: Terminus Tool Scoping

Partner agent uses the same Terminus (CT214) as Lumina, with per-agent scoping:
- `LUMINA_AGENT_ID=partner` in stdio.sh → Terminus returns `partner` from `get_agent_context()`
- Engram queries automatically scoped to `agents/partner/` namespace
- Personal tools (health, learning, finance) scoped to partner's namespace

No changes to server.py needed — LUMINA_AGENT_ID handles this automatically.

## Phase 5: Verification Checklist

- [ ] CT316 running, IronClaw 0.24.0 installed
- [ ] Terminus connected (`tools:249` in startup log)
- [ ] Household messages routing correctly between Lumina and Partner
- [ ] Engram writes scoped to `agents/partner/` namespace
- [ ] PARTNER.md loaded as workspace context
- [ ] Matrix/Signal channel working for partner's household

## Rollback

To remove the partner agent:
1. Stop IronClaw service on CT316
2. Remove CT316 from Proxmox
3. Update `HOUSEHOLD_AGENTS` in household_config.py
4. Archive any partner Engram data

## Notes

- Partner agent costs ~$0.01-0.05/day in LLM inference (Qwen for routine tasks, Claude for complex requests)
- All shared household events broadcast via `household_sync()` in Nexus
- Finance events only route to Lumina by default (configure in household_config.py)
- The partner agent intentionally cannot access Lumina's personal namespace in Engram
