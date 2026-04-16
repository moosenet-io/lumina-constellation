---
name: morning-briefing
description: Generate a personalized morning briefing with weather, calendar, commute, and curated news
version: 1.0
author: moosenet-io
license: MIT
agent: vigil
container: <fleet-host>
schedule: "06:30 weekdays"
tags: [briefing, daily, news, calendar, weather]
---

# Morning Briefing

Generate a comprehensive morning briefing for the operator. Combine data from multiple sources into a readable daily summary.

## Procedure

1. Fetch weather for San Francisco (current + today's forecast)
2. Query Google Calendar for today's events (<operator-personal-email> + Lumina Actions)
3. Estimate commute home→work via TomTom
4. Fetch top headlines filtered by the operator's interests: Tech (AI/ML, gaming, homelab), Finance (crypto, stocks), Politics (with bias ratings), Sports (Giants, Warriors, Sharks, 49ers, Valkyries)
5. Check Nexus inbox for any urgent messages
6. Compose briefing using Qwen local model (cost: $0)
7. Post to Matrix channel and update /briefing/ dashboard

## Inference de-bloat

- Steps 1-5: Pure Python API calls ($0)
- Step 6: Local Qwen model ($0)
- Step 7: Python template rendering ($0)
- No cloud LLM unless news synthesis requires it

## Output format

Briefing dashboard HTML at http://<fleet-server-ip>/briefing/
Matrix message: condensed text version to the operator's room
