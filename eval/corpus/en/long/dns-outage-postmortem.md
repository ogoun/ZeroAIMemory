---
tags: [postmortem, dns, outage, incident]
---
# Postmortem: internal DNS resolution failure across the production network

On a Tuesday afternoon, internal name resolution failed intermittently across the production network for one hour and seventeen minutes. Services that needed to resolve internal names, which is nearly all of them, saw a mix of timeouts and `SERVFAIL` responses. External traffic was affected because the API gateway could not reliably resolve its upstream services, and roughly forty percent of customer requests failed at the worst point. This is a blameless postmortem: the goal is to understand how the system allowed a routine change to cause a large outage, not to find someone to blame. Times are in UTC and come from the incident channel, resolver metrics and change logs.

#### Summary

A configuration change to the internal resolvers, intended to add a forwarding zone for a newly acquired company's domain, was rolled out to all resolver instances at once. The new forwarding target was reachable from only some of our network segments. When resolvers in the other segments received queries for that zone, they waited for the forwarding target until timeout, and because the resolver software processed forwarded queries on a shared, limited pool of worker threads, slow forwarded queries exhausted the pool and blocked resolution of every other name, including our own internal zones. The change itself affected one obscure zone, but the failure mode turned it into an outage for everything.

#### Background

##### How internal DNS works here

Internal resolution is handled by a fleet of twelve recursive resolvers spread across three regions. Every host is configured with two resolvers from its own region in its resolver configuration, plus a local caching stub that runs on each host. The stub caches answers for up to the record's time to live, with a cap of five minutes, and retries the second resolver if the first does not answer within two seconds. Authoritative data for internal zones lives on a separate set of authoritative servers, and the resolvers forward internal zones to them. Queries for public names are resolved recursively from the root.

###### The forwarding change

After the acquisition, engineers needed to resolve names in the acquired company's private zone, which was served by their own DNS servers inside a VPN tunnel. The tunnel terminated in one of our three regions. The change added a conditional forwarder for that zone to the resolver configuration, pointing at the acquired company's servers. In the region with the tunnel, those servers were reachable and the change worked. In the other two regions there was no route to them at all, so packets to them were silently dropped by the network rather than rejected, and every forwarded query waited for the full forwarding timeout before failing.

#### Timeline

##### Before the incident

The change was reviewed and approved the previous day. The review checked the zone name and the forwarder addresses, and the change was tested by resolving a name in the new zone from a host in the region with the tunnel, which worked. Nobody tested from the other regions, because the change was seen as additive: it added a zone, it did not change anything about existing zones, so the worst case seemed to be that the new zone would not resolve from some places.

##### During the incident

At 14:02 the change was deployed to all twelve resolvers through the configuration management system, which applies changes to all instances of a role in parallel by default. At 14:06 the first alert fired for elevated error rates on the API gateway. At 14:09 the on-call engineer for the gateway started investigating and saw upstream connection failures with name resolution errors in the logs. At 14:15 the incident was declared and the networking on-call joined. At 14:21 resolver metrics showed that the number of in-flight queries on resolvers in two regions was pinned at the configured maximum, and that query latency for all zones had risen from under a millisecond to several seconds. At 14:30, someone noticed in the resolver logs that a large share of the in-flight queries were for the new forwarding zone. At 14:34 the decision was made to roll back the change. The rollback had to go through the same configuration management system, which first had to finish its periodic run on the resolver hosts; the rollback was applied at 14:51. Resolver latency recovered within a minute of the rollback on each host, but services recovered more slowly because many of them had cached negative answers or had connection pools full of failed connections. The incident was declared resolved at 15:19, after error rates on the gateway had been at baseline for fifteen minutes.

###### Why the new zone was queried at all

The obvious question is why our services were querying a zone they had never heard of. The answer turned out to be the corporate laptop agent. Every laptop that connected to the VPN after the acquisition got a search domain list that included the acquired company's domain, and several internal tools, including the deploy tool that engineers ran from their laptops against production, resolved short host names by trying each search domain in turn. Those queries went through jump hosts in all three regions. On top of that, a monitoring agent on production hosts did reverse lookups on peer addresses, and the acquired company's address ranges overlapped with one of our old, partially decommissioned internal ranges, so reverse lookups for addresses in that range were forwarded to the new servers as well. Neither path was large on its own, but together they produced a few hundred queries per second for the forwarded zone in each region, which was more than enough to exhaust the worker pool when each query took several seconds to time out.

#### Contributing factors

##### Shared worker pool

The root technical issue is that the resolver software used one shared pool of workers for all queries, with no isolation between forwarded zones and everything else. A slow upstream for one zone could consume all workers. We confirmed this in a test environment by pointing a forwarded zone at a black-hole address and sending fifty queries per second for it: within twenty seconds, resolution of unrelated names on the same resolver started timing out. The resolver supports a per-upstream limit on concurrent queries, which would have contained the damage, but it was not configured. Example of the setting we have now added for every forwarder:

```text
forward-zone:
    name: "acquired.example"
    forward-addr: 10.200.0.53
    forward-first: no
max-forward-concurrency: 32
```

##### Timeouts that were too long

The forwarding timeout was set to the software default, which waits several seconds and retries before giving up. For forwarded zones inside our own network, an answer that does not arrive within a few hundred milliseconds is not going to arrive. We lowered the timeout for forwarders and, more importantly, configured the resolvers to cache failures from a forwarder for a short time, so that a dead upstream causes one slow query per interval rather than one per request.

###### Parallel rollout

The configuration management system applied the change to all twelve resolvers at once. If it had rolled out to one resolver at a time with a health check between them, the first resolver in an affected region would have failed its health check within a minute, and the rollout would have stopped with most of the fleet untouched. The stub resolver on each host would have failed over to the second, healthy resolver. Parallel rollout is a reasonable default for many roles, but for a role whose failure affects everything, it is not.

###### Rollback latency

The rollback took seventeen minutes from decision to effect, because the configuration management agent on the resolver hosts runs on a thirty-minute schedule and the system does not support forcing an immediate run for a single role without an extra approval step that was not documented in the runbook. The approver for that step was in a meeting. We have since added the resolver role to the list of roles that support an emergency push, with a documented approval path that the incident commander can grant.

#### What went well

Several things worked as intended. The local caching stub on each host meant that names resolved recently continued to work for up to five minutes, which is why the outage was partial rather than total in the first minutes. The gateway's alert fired within four minutes of the change, which is fast. Once the right people were in the channel, the diagnosis took about fifteen minutes, mostly because resolver metrics clearly showed in-flight queries pinned at the maximum.

#### Action items

##### Completed

- Per-forwarder concurrency limits and short timeouts on all resolvers.
- Negative caching of forwarder failures for thirty seconds.
- Resolver role switched to rolling deployment, one instance at a time, with a health check that resolves a set of names from every internal zone and every forwarded zone.
- Emergency push path documented and tested for the resolver role.
- The overlapping address range was fully decommissioned and removed from reverse zones.

##### In progress

- Moving forwarded zones for external parties onto a separate resolver pool, so that no third-party DNS server can ever affect resolution of our own zones.
- Changing the laptop VPN configuration so that search domains for acquired companies are only pushed to people who need them.
- Adding a synthetic check that resolves a set of critical internal names from every region every ten seconds, with an alert on latency, not only on failures.

#### Testing the change before rollout

A change like this one needs to be tested from every place that will run it, not only from the place it was written for. We have added a pre-deployment check to the DNS change pipeline: it starts a resolver with the proposed configuration in each region's test network and runs a standard query set against it, including queries for the new zone, for existing internal zones and for public names, and it measures latency as well as correctness. Run the same check by hand before proposing a change:

```sh
dnscheck run --config resolver.conf --regions all --queryset standard,changed-zones
```

The check would have caught this incident: in two of the three regions, queries for the new zone take several seconds, and the check fails any query set where the 99th percentile latency for any zone exceeds 200 ms.

###### Note on stub resolvers

During the incident some services recovered much later than others even after resolvers were healthy. The slowest to recover were services written in a runtime whose default resolver ignores the host's caching stub and queries the configured servers directly, with no caching of its own; these services had also been configured years ago with a single resolver address rather than two, so they had no failover either. We found eleven such services. All have been changed to use the host stub.

#### Lessons

The change looked additive and low-risk, and in isolation it was. The outage came from three properties of the system that had nothing to do with the change: a shared resource with no isolation, timeouts far longer than any useful answer could take, and a rollout mechanism that applied a change everywhere at once. Each of those properties had been present for years without causing trouble, and each of them is the kind of thing that only matters when something else goes wrong. When reviewing changes to shared infrastructure, we now ask not only "what does this change do" but also "what happens to everything else if the new thing is slow or unreachable." The second lesson is about knowing your traffic. Nobody on the team expected production systems to send any queries to the acquired company's zone, and that expectation was the basis for treating the change as safe. A five-minute look at resolver query logs, filtered by the zone name in a test that simply logged queries without forwarding them, would have shown otherwise. We now run new forwarding zones in log-only mode for a day before enabling forwarding, which tells us who queries them and how often before any real dependency is created.
