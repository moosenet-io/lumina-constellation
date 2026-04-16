# Lumina vs crewAI: Orchestration Patterns

Two fundamentally different philosophies for multi-agent coordination. Understanding the difference explains why Lumina is built the way it is.

---

## The Core Difference

**crewAI**: Peer-to-peer delegation. Agents spawn sub-agents, pass work laterally, chain outputs. Each agent is a full autonomous actor that can delegate and receive delegation from any peer.

**Lumina**: Hub-and-spoke orchestration. One orchestrator (Lumina) coordinates everything. Sub-agents are specialized workers with narrow responsibility. No agent can message another agent directly — everything routes through the hub.

```
crewAI:                     Lumina:
Agent A ↔ Agent B           Lumina ← Nexus ← Axon
   ↕           ↕               ↓
Agent C ↔ Agent D           Nexus → Vigil
```

---

## Why Lumina Chose Hub-and-Spoke

### 1. Observability

In crewAI, work fans out across many agents. Tracing why a decision was made requires reconstructing a graph of agent-to-agent calls. In Lumina, Lumina sees every step. Every delegation is explicit (`nexus_send()`), every result comes back to Lumina before anything else acts on it.

This matters because the operator needs to understand what happened without reading logs. Lumina can explain the chain of events because it was the only agent that touched all of them.

### 2. Budget Control

crewAI's peer agents make independent inference decisions. Cost accumulates laterally — one agent spawning sub-agents can drive unexpected expense. In Lumina, every LLM call either goes through Lumina (and Lumina decides how much to spend) or through a fleet agent running on strict quota.

The Obsidian Circle is the only place where Lumina deliberately allocates budget to multiple models. Even then, budget is an explicit parameter with a hard ceiling enforced per-member.

### 3. No Circular Delegation

With peer delegation, Agent A can delegate to B, who delegates to C, who delegates back to A. In practice this creates deadlocks and infinite loops that are hard to detect. Hub-and-spoke makes circular delegation structurally impossible — agents don't have addresses that other agents can call.

### 4. Personality Coherence

Lumina has a persistent persona (voice, tone, relationship with the operator). If other agents could respond to the operator directly, that coherence breaks — the operator might get responses from Vigil, Axon, and Sentinel in three different styles. In Lumina, every user-facing response goes through Lumina's voice.

### 5. Deployability

Peer agents need service discovery — they need to know where other agents live, health-check each other, handle failures. Hub-and-spoke only requires agents to know where Nexus (the inbox) is. Adding a new agent doesn't require updating any other agent's config.

---

## Comparison Table

| Dimension | crewAI | Lumina |
|-----------|--------|--------|
| Communication | Peer-to-peer (any→any) | Hub-and-spoke (via Nexus) |
| Orchestration | Distributed | Centralized (Lumina) |
| Delegation | Agents spawn sub-agents | Lumina delegates to fleet |
| State | Each agent manages own | Nexus is the shared state |
| Observability | Requires distributed tracing | Lumina sees all |
| Cost control | Per-agent, independent | Lumina-gated |
| Adding an agent | Update multiple agents | Add `.agent.yaml`, wire Nexus |
| Failure handling | Complex cascades | Lumina retries or escalates |
| Circular delegation | Possible | Structurally impossible |
| User-facing voice | Any agent can respond | Always Lumina |

---

## What Lumina Sacrifices

Hub-and-spoke has real trade-offs:

**Throughput**: In crewAI, dozens of agents can work in parallel without going through a hub. Lumina's Axon can run parallel tasks, but Lumina itself remains the bottleneck for coordination decisions. For a small operator setup (one person, one orchestrator), this is not a constraint. For a 100-user enterprise platform, it would be.

**Latency on multi-hop tasks**: If task A needs B's output which needs C's output, crewAI can pipeline them. In Lumina, each step routes through Nexus, adding round-trip overhead. Acceptable for async, background tasks. Not ideal for real-time chains.

**Agent autonomy**: crewAI agents make their own decisions about what to delegate and how. Lumina's agents are less autonomous — they do what the work order says. This is intentional (predictability over autonomy) but means agents can't adapt to novel situations without Lumina involvement.

---

## The Obsidian Circle as a Hybrid

The Circle is the one place where Lumina uses a peer-style pattern internally. During a `convene()` call:
- Multiple models reason in parallel
- Earlier members' outputs are broadcast to later members (a form of lateral information sharing)
- Mr. Wizard synthesizes without being "above" the other members

But this is **bounded**: it only happens inside a single `convene()` call, with explicit budget, and always returns a single result to Lumina. It's peer deliberation with a hard perimeter. The Circle doesn't spawn new agents, can't call Nexus, and has no persistence beyond the session checkpoint.

This is the design pattern Lumina uses when it genuinely needs multi-perspective reasoning: contained, budgeted, time-boxed, returning a single action guidance.

---

## When to Reconsider

If Lumina ever needs to:
- Support 10+ concurrent operators with independent work streams
- Run truly autonomous agents for hours without human involvement
- Pipeline dozens of heterogeneous tasks in parallel

...then the hub-and-spoke model should be revisited. Those are signs that a distributed agent mesh (closer to crewAI) might make more sense. For the current scope (one operator, ~5-10 background agents, async work orders), the simplicity of hub-and-spoke pays significant maintenance dividends.
