# Monitoring with Prometheus and Grafana

Optional. TeamCraft's own health checks and alerts (docs/operations.md, "Health and alerts") work
without any of this; these files are for operators who already run Prometheus.

| File                       | What                                                         |
|----------------------------|--------------------------------------------------------------|
| `teamcraft-alerts.yml`     | Alert rules: health checks, targets down, 5xx, pool, push    |
| `teamcraft-dashboard.json` | Grafana dashboard over the metrics of docs/operations.md     |

Scrape the metrics listeners (off by default, loopback by default) with the job names the rules
expect:

```yaml
scrape_configs:
  - job_name: teamcraft-server
    static_configs: [{ targets: ["127.0.0.1:9464"] }]
  - job_name: teamcraft-sfu
    static_configs: [{ targets: ["127.0.0.1:9465"] }]
  - job_name: teamcraft-bridge
    static_configs: [{ targets: ["127.0.0.1:9466"] }]
```

Import the dashboard in Grafana (Dashboards, New, Import) and pick the Prometheus data source.
Add a probe of `https://<your host>/readyz` from another machine as well (the Blackbox exporter or
any uptime check): nothing on the server can report that the server is down.
