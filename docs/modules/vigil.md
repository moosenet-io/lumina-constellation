# Vigil — Morning & Evening Briefings

Vigil runs on <fleet-host> as a Python systemd timer. Every morning at a configured time, it gathers data from 12+ sources and synthesizes a personalized briefing.

## What it collects

- **Weather**: home and work location current conditions via wttr.in
- **Commute**: TomTom traffic data vs baseline
- **News**: Tech, business, general via NewsAPI
- **Calendar**: Google Calendar events via CalDAV
- **Tasks**: High-priority open Plane items
- **Renewals**: Upcoming document/insurance expirations from Relay
- **Health**: Myelin daily cost summary
- **Sports**: SF Giants, Warriors, Sharks, 49ers, Valkyries

## How it works

1. Parallel data collection (ThreadPoolExecutor)
2. Raw data written to Gitea as JSON
3. Each section formatted via Qwen local model OR templates
4. HTML dashboard generated using constellation.css
5. Matrix message sent with link + text summary

## Configuration

Vigil reads Google credentials from Infisical via axon/.env.
Calendar ID: set `GOOGLE_LUMINA_CALENDAR_ID` in axon/.env.
Briefing time: edit the commute-morning.timer on <fleet-host>.

## Inference cost

Briefing synthesis: **$0** (Qwen local model, 0 cloud calls).
