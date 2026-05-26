# portunus-standalone on Kubernetes

These manifests run `portunus-standalone` as a single-replica Deployment
with `hostNetwork: true`. This is intentional — `portunus-standalone`
forwards arbitrary TCP/UDP ports (including ranges), which the
ClusterIP / NodePort / LoadBalancer service abstractions cannot express.

## Topology choices

| Topology | When to use |
| --- | --- |
| Deployment, replicas=1 (this template) | One forwarder pinned to a chosen node. Most common. |
| DaemonSet                              | One forwarder per node — fan out from each edge node. Change `kind: Deployment` → `kind: DaemonSet` and remove `replicas`/`strategy`. |
| Multiple replicas with bridge networking | Not supported — Service abstractions cannot route arbitrary listen ports. |

## Why `hostNetwork: true`

The Pod must bind on the node's actual network interfaces; otherwise
upstream targets cannot reach the listener and source-address-based
flow tracking (UDP) is broken by NAT.

**Cluster restrictions**: many production clusters reject `hostNetwork`
via PodSecurityPolicy / Pod Security Admission. Coordinate with your
platform team before applying.

## Apply

```sh
kubectl apply -f configmap.yaml
kubectl apply -f deployment.yaml
kubectl logs -l app=portunus-standalone --tail=50
```

## Updating the config

Editing the ConfigMap does **not** trigger a Pod restart — the standalone
binary reads the file once at startup. After `kubectl apply -f
configmap.yaml`, also run:

```sh
kubectl rollout restart deployment/portunus-standalone
```
