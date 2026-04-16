# Installation

## Requirements

- virtualization platform 8.0+ (or any Linux with Docker)
- 4GB RAM minimum (8GB recommended)
- 20GB disk
- Internet connection for model downloads

## Standalone (Docker, no virtualization)

```bash
git clone https://github.com/moosenet-io/lumina-deploy.git
cd lumina-deploy
cp .env.example .env
nano .env  # Add your ANTHROPIC_API_KEY
docker compose up -d
```

Open http://localhost:443 and complete the Soma wizard.

## virtualization homelab (full stack)

See [self-hosted deployment setup](virtualization-setup.md) for multi-service deployment with dedicated runtime targets for each module.

## Verify installation

```bash
# Check all services are healthy
curl http://localhost:8080/api/health -H "X-API-Key: your-key"

# Check Soma admin
curl http://localhost:8082/health
```
