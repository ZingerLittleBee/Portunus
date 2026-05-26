# portunus-standalone — Deployment templates

Reference templates for running `portunus-standalone` in production.
None of these files are required by the binary itself; they exist as
copy-pasteable starting points.

| File | Purpose |
| --- | --- |
| `portunus.example.toml`         | Runnable TOML config with TCP/UDP/range/multi-target/PROXY-protocol examples |
| `portunus-standalone.service`   | Hardened systemd unit (CAP_NET_BIND_SERVICE, NoNewPrivileges, ProtectSystem) |
| `Dockerfile`                    | Multi-stage build → distroless runtime image |
| `docker-compose.yml`            | Single-service compose using host networking |
| `k8s/configmap.yaml`            | Kubernetes ConfigMap holding `standalone.toml` |
| `k8s/deployment.yaml`           | Kubernetes Deployment (hostNetwork: true, 1 replica) |
| `k8s/README.md`                 | hostNetwork vs DaemonSet vs NodePort notes |

Full usage and `[global]/[defaults]/[[rule]]` schema reference is in
[`docs/content/docs/operations/standalone.mdx`](../../../docs/content/docs/operations/standalone.mdx).
