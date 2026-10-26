---
tags: [kubernetes, upgrade, operations, log]
---
# Kubernetes control plane and node upgrade: working log for three minor versions

We run four production clusters and two staging clusters on self-managed Kubernetes. By the start of this quarter they were three minor versions behind the oldest version still supported upstream, which meant no security patches and a growing list of deprecated APIs in use. This log covers the upgrade of all six clusters across three minor versions, one minor version at a time, as the version skew policy requires. It is written in roughly chronological order and keeps the dead ends, because the dead ends are what the next upgrade will need to know about. Cluster names are replaced with their roles.

## Preparation

#### Deprecated APIs

The first job was finding every use of an API version that would be removed in the versions we were moving to. Removed API versions are the main way an upgrade breaks workloads: the control plane simply stops serving them, and any manifest, controller or client that still uses them fails. We used three sources. The API server's metrics show requests per API group and version, including deprecated ones, and the audit log shows which user agent made them. A static scan of all manifests in our deployment repositories found the rest, including manifests for things that are only applied occasionally, like quarterly batch jobs. Run this against a cluster to list requests to deprecated APIs since the last API server restart:

```sh
kubectl get --raw /metrics | grep apiserver_requested_deprecated_apis
```

The scan turned up about two hundred manifests using removed versions, nearly all of them for ingress and pod disruption budget objects, plus three in-house controllers built against an old client library that used a removed version of the custom resource definition API. The manifests were mechanical to fix. The controllers were not, because one of them was maintained by a team that had since been reorganized, and nobody had built it in over a year. Rebuilding it with a current client library took a week, mostly to recreate its build environment.

##### Add-ons and their compatibility

Every add-on in the cluster has its own compatibility matrix with Kubernetes versions: the network plugin, the storage drivers, the ingress controller, the certificate manager, the metrics pipeline, the policy engine and the autoscalers. We built a table with each add-on, its current version, the range of Kubernetes versions it supports and the version we needed for each step. Two add-ons needed to be upgraded before the first Kubernetes step, because their current versions did not support the target version, and one, the old storage driver for our block volumes, did not support any version past the second step at all and had to be replaced with the newer driver before we could finish.

## Staging first

#### First staging upgrade

The first staging cluster was upgraded one minor version on a Monday. The control plane upgrade itself went smoothly: the three control plane nodes were upgraded one at a time, each drained of its static pods, upgraded, and returned, and the API stayed available throughout because the load balancer in front of the API servers removed each node while it restarted. The node upgrade was a different story. We upgrade nodes by replacing them, not in place: a new node group with the new version is created, old nodes are cordoned and drained one at a time, and the old group is deleted when empty.

##### Drains that never finish

Drains got stuck on about one node in five. The cause in almost every case was a pod disruption budget that could not be satisfied: a deployment with one replica and a budget that required at least one available pod, which makes the pod impossible to evict voluntarily. These budgets had been created by a deployment template that set `minAvailable: 1` for every service regardless of its replica count. For services with three or more replicas this is fine, but for single-replica services it blocks every drain forever. We changed the template to use `maxUnavailable: 1` instead, which always allows at least one eviction, and found eighty-one single-replica services in staging and about a hundred and thirty in production that needed the change. Some of those services should not have had one replica in production in the first place, and fixing that became a separate project. Example of the budget we now generate by default:

```yaml
apiVersion: policy/v1
kind: PodDisruptionBudget
metadata:
  name: catalog-search
spec:
  maxUnavailable: 1
  selector:
    matchLabels:
      app: catalog-search
```

#### Second and third steps in staging

The second step in staging surfaced a change in how the kubelet handled a deprecated flag for image garbage collection thresholds: the flag was removed, and nodes started with defaults that kept far more unused images on disk. On our nodes with small root disks, image storage filled the disk within two days, and nodes started evicting pods for disk pressure. The fix was to move the thresholds into the kubelet configuration file, where they belong, but the problem only appeared after two days of normal operation, which is why it took us a while to connect it to the upgrade. The third step in staging was uneventful apart from the storage driver replacement, which we did as a separate change a week before.

## Production

#### Order of clusters

Production clusters were upgraded in order of increasing risk: the internal tools cluster first, then the batch processing cluster, then the secondary region's serving cluster, and finally the primary region's serving cluster. Each step waited at least one week on each cluster before the next step started anywhere, so that slow problems like the image garbage collection one had time to show up.

##### The batch cluster and long-running jobs

The batch cluster was harder to drain than any serving cluster, because it runs jobs that take hours and cannot be interrupted without losing their work. Draining a node would kill those jobs. We added a node annotation that the batch scheduler respects: when a node is marked for upgrade, the scheduler stops placing new jobs on it, and the drain only starts when no job on the node has more than fifteen minutes of expected remaining time. For the longest jobs, which run for up to eleven hours, this meant some nodes waited most of a day before they could be drained. The whole batch cluster took four days per step instead of the one day we had planned, but no job was lost.

#### Primary region serving cluster

The primary serving cluster has about three hundred nodes and runs everything customer facing. The control plane upgrades were done in the early morning, and the node replacement ran over three days per step, at a rate of about four nodes per ten minutes during business hours and faster at night. We watched error rates and latency for each service during the replacement, and the dashboards were set to show node replacement events as annotations.

##### A surprise with the ingress controller

On the second step in production, about two hours into the node replacement, error rates for a small set of services spiked briefly every time an ingress controller pod was rescheduled. The new ingress controller version, which we had upgraded as a prerequisite, terminated connections on shutdown faster than the old one: the old version waited for in-flight requests for up to thirty seconds, while the new one used a shorter default grace period that cut off long-polling connections. The affected services used long polling for order status updates in the mobile app. We restored the longer grace period in the ingress controller configuration and paused the node replacement until the change was rolled out. Example of the setting:

```yaml
controller:
  config:
    worker-shutdown-timeout: "30s"
  terminationGracePeriodSeconds: 45
```

The pod's termination grace period must be longer than the controller's own shutdown timeout, otherwise Kubernetes kills the pod before the controller finishes draining, and the setting has no effect. That detail is easy to miss and is now in our ingress runbook.

##### etcd during the upgrade

The etcd cluster behind each control plane deserves its own note. Before each control plane step, we took an etcd snapshot and verified that it could be restored into a scratch cluster, because a failed control plane upgrade that corrupts etcd is the one scenario that node replacement does not protect against. Restoring the snapshot of the primary cluster took about six minutes, and the restored scratch cluster listed the same objects as production, which we checked by comparing object counts per resource type. During the third step, etcd reported a database size close to its configured quota on the primary cluster, which turned out to be caused by an event-heavy controller generating millions of event objects with a long time to live. We raised the quota, compacted and defragmented the members one at a time, and fixed the controller to emit fewer events, after which the database size dropped from 6.8 GB to about 1.9 GB.

#### Communication with service teams

We announced each production step a week ahead, with the dates of node replacement per cluster and a short list of things teams should check: disruption budgets, single-replica deployments, pods with local storage and long-lived connections. Teams that had any of these were contacted directly with the list of affected workloads from the pre-flight report. This direct contact worked much better than the general announcement, which most people skimmed: of the forty teams with affected workloads, thirty-three fixed them before the production steps started, and the remaining seven were handled by the platform team with their agreement.

## Things that went well

The version skew policy, which allows nodes to lag the control plane by up to a few minor versions, gave us flexibility we used repeatedly: when the ingress problem appeared, the control plane was already upgraded and nodes were half replaced, and we could simply leave that mixed state in place for two days while we fixed the configuration. Node replacement instead of in-place upgrades meant that every step was trivially reversible until the old node group was deleted, which we only did after the new nodes had run for twenty-four hours without problems.

#### Automation we added

Two pieces of automation came out of this upgrade and will make the next one faster. The first is a pre-flight job that runs before any cluster upgrade and reports deprecated API usage from metrics and audit logs, add-on compatibility from a maintained matrix, pod disruption budgets that block eviction, and nodes with local storage that would be lost on replacement. The second is the drain orchestrator, which replaces nodes at a configurable rate, respects the batch scheduler's annotation, pauses automatically when error rates for any service cross a threshold, and posts progress to the upgrade channel. Run the pre-flight check against a cluster with:

```sh
cluster-preflight --cluster prod-primary --target-version next --report preflight.html
```

###### Timeline in numbers

The whole project took eleven weeks: three weeks of preparation, two weeks of staging, and six weeks of production upgrades across three steps and four clusters. Engineering effort was roughly two engineers full time plus help from service teams for the disruption budget changes and the old controller.

## Lessons

Upgrade more often. Three minor versions at once turned a routine task into a quarter-long project, mostly because problems compound: each removed API, each add-on incompatibility and each changed default had accumulated over time, and we had to deal with all of them at once. Our new policy is to upgrade every cluster within three months of a new minor version being released upstream, so that each upgrade is one step and the list of surprises is short. The pre-flight job runs weekly against all clusters, not only before upgrades, so that new uses of deprecated APIs are caught as they are introduced rather than discovered a year later. Second, test with time, not only with traffic. The image garbage collection problem took two days to appear, and the ingress problem only appeared under real long-polling traffic. A staging upgrade that is declared successful an hour after it finishes will miss exactly the problems that hurt most in production. Third, the most time-consuming part of a cluster upgrade is often not Kubernetes at all, but the workloads and add-ons running on it, and especially software that nobody owns any more. The rebuilt controller and the single-replica services with blocking disruption budgets cost more time than every control plane step combined. An inventory of owners for every controller and add-on, kept up to date between upgrades, would have saved weeks.
