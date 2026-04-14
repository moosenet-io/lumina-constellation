# First Five Minutes

What to do after `docker compose up` completes and Soma is running.

## 1. Open the Admin Panel

Navigate to `http://localhost:8082` (or `https://yourdomain` if behind Caddy).

You should see the Soma status dashboard. If you see a 401 error, the `SOMA_SECRET_KEY` env var is set — you'll need to pass `X-Soma-Key` in the header. For the web UI, this is handled automatically via the login flow.

## 2. Run the Naming Ceremony

Lumina needs a name. Open a terminal and run:

```bash
docker exec -it lumina-fleet python3 /opt/lumina-fleet/naming_ceremony.py
```

This creates `constellation.yaml` with your preferred agent display names. You can also rename agents later through Soma's **Config** tab.

## 3. Check Module Status

In Soma, click **Status**. You should see all active modules with their health indicators:

- **Green dot** — running normally
- **Yellow dot** — degraded or last check failed
- **Red dot** — down

If any module is red, check the **Logs** tab for that module's systemd journal output.

## 4. Connect Your Chat Platform

Lumina communicates through Matrix by default. In Soma, go to **Config > Communication** and enter:

- Matrix homeserver URL (e.g., `http://matrix.yourdomain:8008`)
- Bot account username and password
- Your personal Matrix user ID (to receive messages)

After saving, test with: `curl http://localhost:8082/api/health`

## 5. Configure Your AI Provider

Go to **Config > AI Providers**. At minimum, you need one:

| Provider | Use case | Setup |
|----------|----------|-------|
| **LiteLLM (local)** | All routine routing | URL: `http://litellm:4000` |
| **Anthropic** | Reasoning tasks (Sonnet/Opus) | API key from console.anthropic.com |
| **OpenRouter** | Multi-model (Wizard council) | API key from openrouter.ai |
| **Ollama** | Local inference ($0) | Auto-discovered if in same Docker network |

## 6. Enable Your First Module

Go to **Config > Modules** and enable **Vigil** (briefings). Click **Enable**, then configure:

- Your location (for weather)
- Google Calendar CalDAV URL (or leave blank for a minimal briefing)
- News API key (free tier works)

Trigger a test briefing:

```bash
docker exec lumina-fleet python3 /opt/lumina-fleet/vigil/briefing.py --test
```

You should see output in the terminal and (if Matrix is configured) a message in your Matrix room.

## 7. Verify the Inbox (Nexus)

Nexus is the message bus. Verify it's working:

```bash
curl -H "X-Soma-Key: your-key" http://localhost:8082/api/health
```

If Postgres isn't running yet (minimal profile), Nexus runs in SQLite fallback mode. For production, enable the Postgres service.

## Common First-Run Issues

| Issue | Fix |
|-------|-----|
| Soma won't start | Check `SOMA_SECRET_KEY` env var and port 8082 availability |
| Modules show red | Check `/opt/lumina-fleet/constellation.yaml` exists |
| Briefing fails | Verify network access to weather/news APIs from the container |
| Matrix not connecting | Confirm homeserver URL and bot account credentials |
| Naming ceremony fails | Run `pip install pyyaml` in the container first |

## Next Steps

- [Architecture Overview](../architecture/constellation-overview.md) — Understand how it all fits together
- [Adding MCP Tools](../guides/adding-tools.md) — Extend Terminus with new capabilities
- [Module Index](../modules/index.md) — Deep-dive into any specific module
