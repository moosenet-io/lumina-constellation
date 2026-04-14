# Adding a New Agent

With portable agent definitions (NPC Feature 1), adding an agent is straightforward.

## 1. Create the .agent.yaml

```bash
cp /opt/lumina-fleet/agents/lumina.agent.yaml /opt/lumina-fleet/agents/myagent.agent.yaml
nano /opt/lumina-fleet/agents/myagent.agent.yaml
```

Minimum required fields:
```yaml
name: myagent
display_name: My Agent
description: What this agent does
routes:
  - type: litellm
    model: Lumina Fast
    enabled: true
engram:
  namespace: agents/myagent
container: CT310
runtime: subprocess
```

## 2. Verify loading

```python
from agent_loader import AgentLoader
loader = AgentLoader()
print(loader.get('myagent').display_name)  # My Agent
```

## 3. Deploy container (if new CT)

Follow the [partner agent onboarding runbook](../household/partner-onboarding-runbook.md).

## 4. Wire to Nexus

```python
from nexus.household_routing import send_household
# Or use nexus_send() via MCP tools
```

## 5. Test

```bash
python3 /opt/lumina-fleet/shared/agent_loader.py
# Should show your new agent in the list
```
