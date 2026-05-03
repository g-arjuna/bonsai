# External Service Infrastructure

Bonsai integrates with several external services for enrichment and event forwarding.
This document describes how to bring up the complete external stack for development and
end-to-end testing using a single command.

## One-command bring-up

```bash
# 1. Copy and edit .env
cp .env.example .env
# Set: BONSAI_VAULT_PASSPHRASE, SPLUNK_PASSWORD, SPLUNK_HEC_TOKEN

# 2. Generate TLS certificates for the bonsai core compose
scripts/generate_compose_tls.sh

# 3. Start all external services
docker compose -f docker/compose-external.yml --profile all up -d

# 4. Seed all services with lab topology data
scripts/seed_external.sh

# 5. Verify everything is up and seeded (machine-readable JSON)
scripts/check_external.sh | jq .

# 6. Start bonsai
docker compose --profile two-collector up -d

# 7. (Optional) Generate bonsai.toml enrichment/adapter sections
scripts/configure_external.sh
```

Total time: approximately 5-10 minutes (dominated by NetBox and Splunk startup).

## Service URLs

| Service | URL | Notes |
|---------|-----|-------|
| NetBox API | http://localhost:8000/api/ | Token: `bonsai-dev-token` |
| NetBox UI | http://localhost:8000 | admin / bonsai-dev |
| Splunk Web | http://localhost:8100 | admin / `$SPLUNK_PASSWORD` |
| Splunk HEC | http://localhost:8088/services/collector | Token: `$SPLUNK_HEC_TOKEN` |
| Elasticsearch | http://localhost:9200 | No auth (dev mode) |
| Kibana | http://localhost:5601 | No auth (dev mode) |
| Prometheus | http://localhost:9093 | Includes remote-write receiver |
| Grafana | http://localhost:3001 | admin / admin — pre-loaded bonsai dashboard |
| Bonsai HTTP | http://localhost:3000 | (bonsai itself, not a compose-external service) |

## Individual profiles

Start only what you need:

```bash
# NetBox only (for enrichment development)
docker compose -f docker/compose-external.yml --profile netbox up -d

# Splunk + Elastic only (for output adapter testing)
docker compose -f docker/compose-external.yml --profile splunk --profile elastic up -d

# Prometheus + Grafana only (for metrics visualisation)
docker compose -f docker/compose-external.yml --profile prometheus up -d
```

## Seed scripts

Each service has its own seed script, all driven from `lab/seed/topology.yaml`:

| Script | Service | What it creates |
|--------|---------|-----------------|
| `scripts/seed_netbox.py` | NetBox | Sites, manufacturer, device types, devices, interfaces, prefixes |
| `scripts/seed_splunk.py` | Splunk | Indexes `bonsai-events`, `bonsai-metrics`; verifies HEC token |
| `scripts/seed_elastic.py` | Elasticsearch | Index templates, `bonsai-detections`, `bonsai-metrics`; ECS mapping |
| `scripts/seed_servicenow_pdi.py` | ServiceNow PDI | CI records for each lab device (requires PDI credentials) |
| `scripts/seed_external.sh` | All | Orchestrator — runs all of the above in order |

All seeds are idempotent (safe to re-run).

## Health check JSON

`scripts/check_external.sh` emits machine-readable JSON to stdout:

```json
{
  "netbox":   {"reachable": true,  "seeded": true,  "device_count": 4},
  "splunk":   {"reachable": true,  "hec_token_valid": true},
  "elastic":  {"reachable": true,  "cluster_status": "green", "index_present": true, "bonsai_detections_count": 1},
  "prometheus": {"reachable": true, "scraping_bonsai": false, "bonsai_metric_series": 0},
  "servicenow_pdi": {"reachable": false, "reason": "SNOW_INSTANCE_URL not set"},
  "bonsai":   {"reachable": true,  "device_count": 4, "http": "http://localhost:3000"}
}
```

`prometheus.scraping_bonsai` is `false` until bonsai is running and Prometheus has scraped it.
`servicenow_pdi` is always `false` unless you provide PDI credentials in `.env`.

## Bonsai config generation

After seeding, `scripts/configure_external.sh` reads `.env` and the running service state,
then writes `docker/configs/core.toml.generated` with the appropriate enrichment and adapter
TOML sections pre-filled. Merge these into `bonsai.toml` or the active compose config TOML.

```bash
source .env && scripts/configure_external.sh
# output: docker/configs/core.toml.generated
```

## ServiceNow PDI

ServiceNow PDIs (Personal Developer Instances) are cloud-hosted — no local container.
Provision a free PDI at https://developer.servicenow.com, then:

```bash
# In .env:
SNOW_INSTANCE_URL=https://devXXXXX.service-now.com
SNOW_USERNAME=admin
SNOW_PASSWORD=<your-pdi-password>

# Then run:
source .env && scripts/seed_servicenow_pdi.py
```

## Stopping and cleaning up

```bash
# Stop all external services (preserve volumes)
docker compose -f docker/compose-external.yml --profile all down

# Stop and wipe all data (fresh start)
docker compose -f docker/compose-external.yml --profile all down -v
```

## Troubleshooting

**NetBox `502 Bad Gateway`**: the service is still starting. Wait 60s and retry.

**Splunk HEC returns `{"text":"Token disabled","code":1}`**: the container is up but HEC hasn't
finished initialising. Wait 30s and retry `scripts/seed_splunk.py`.

**Elasticsearch `Connection refused`**: allow 60s for JVM startup. ES_JAVA_OPTS is set to
`-Xms512m -Xmx512m` to limit heap; reduce if the container OOM-kills on low-RAM laptops.

**Prometheus `scraping_bonsai: false`**: Prometheus scrapes bonsai at `host.docker.internal:9090`.
This works on Linux with the `extra_hosts: host-gateway` binding in compose-external.yml.
Start bonsai and wait one 15s scrape interval.

**Port conflict**: bonsai uses 3000 (HTTP) and 9090 (metrics). External services use:
- NetBox: 8000, Splunk: 8100+8088, Elastic: 9200+5601, Prometheus: 9093, Grafana: 3001.
No conflicts with a standard bonsai setup.
