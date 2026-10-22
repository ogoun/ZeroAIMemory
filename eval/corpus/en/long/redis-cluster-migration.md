---
tags: [redis, migration, caching, sessions]
---
# Migrating from a single Redis primary to Redis Cluster

For four years the storefront ran on one Redis primary with two replicas behind Sentinel. It held three very different workloads: a read-through cache for product pages, the session store, and a handful of rate limiters and distributed locks. By last spring the primary was using 58 GB of its 64 GB of memory at peak, a single core was saturated during traffic spikes, and every failover caused a noticeable blip in checkout errors. This document is the working log of moving those workloads to Redis Cluster, written up afterwards so the next team does not have to rediscover the same problems. It is long because the migration was long; the section on multi-key operations is the one most people will need.

## Why not just a bigger box

The obvious first option was vertical scaling. We could have moved to an instance with 128 GB of memory and bought ourselves another year or two. We did not, for two reasons. First, Redis executes commands on a single thread, so more memory does nothing for the saturated core; the CPU problem would have stayed exactly where it was. Second, a bigger single primary makes failover worse, not better: a replica has to load a larger dataset before it can serve traffic, and a full resynchronisation after a network partition takes longer and puts more pressure on the primary. We measured a full resync of the existing dataset at about eleven minutes, during which the primary's latency roughly doubled. Splitting the dataset across shards attacks both problems, at the cost of giving up multi-key operations that span keys on different shards.

### Separating the workloads first

Before touching the cluster at all, we split the three workloads onto separate deployments, because they have completely different requirements. The page cache can be lost entirely without harm beyond a few minutes of slower pages; it needs memory and throughput but not durability. Sessions must survive failover and should survive restarts. Rate limiters and locks are tiny but extremely latency-sensitive and must never be evicted. Keeping all three in one instance meant one eviction policy for everything, and in practice that policy was `noeviction`, which meant the cache could not shed load gracefully when memory ran out. After the split, the cache runs with `allkeys-lru`, sessions run with `volatile-ttl` and append-only persistence, and the locks run on a small dedicated primary with no eviction at all.

#### Measuring key distribution

Redis Cluster places keys into 16384 hash slots by CRC16 of the key, or of the hash tag in braces if one is present. If a few keys are enormous, or if many hot keys share a hash tag, one shard ends up doing most of the work. Run this against a replica to sample big keys before planning shard sizes:

```sh
redis-cli -h cache-replica-1 --bigkeys -i 0.01
redis-cli -h cache-replica-1 --memkeys -i 0.01
```

The scan found one surprise: a single sorted set used for the "recently viewed" feature had grown to 1.9 GB because the trimming job that capped each user's list had been silently failing for months after a deploy changed its schedule format. Every user's recent views lived in one key, keyed internally by user id. In a cluster that key would have pinned 1.9 GB and a large share of reads to whichever shard owned its slot. We rewrote the feature to use one small sorted set per user, which distributes naturally, and fixed the trimming job. The memory used by that feature dropped to about 300 MB once the per-user caps were enforced again.

## Client changes

### Cluster-aware clients

A cluster client has to understand redirects: when a key's slot lives on a different node, the node replies with `MOVED` and the client must retry against the right node and update its slot map. Most of our services used a client library that supports this, but two older services used a thin wrapper that opened a plain connection to a single address. Those wrappers would have worked against a cluster for exactly the keys that happened to live on the node they connected to, and failed randomly for everything else, which is the worst possible kind of failure to debug. We replaced the wrappers before the cutover and added a startup check: every service logs whether its Redis client is running in cluster mode, and a dashboard lists any service that connects to a cluster endpoint without cluster mode enabled.

#### Connection counts

Cluster clients open connections to every primary, and often to replicas too if reads are routed there. With six shards and two replicas each, a client that reads from replicas holds eighteen connections per pool instead of three. Multiply that by a few hundred pods and connection counts on each node jumped from around 2,000 to almost 9,000 in the first staging test. Redis handles that many connections, but our managed proxy layer in front of staging had a per-node connection limit of 10,000, and we were uncomfortably close. We reduced pool sizes for services that did not need them and moved replica reads to only the two services where read volume justified it. Example client configuration we settled on for most services:

```yaml
redis:
  mode: cluster
  seeds: [cache-0:6379, cache-1:6379, cache-2:6379]
  pool_size: 4
  read_from: primary
  max_redirects: 3
```

## Multi-key operations

This was the hardest part, and the reason this document exists. In a cluster, any command that touches multiple keys only works if all the keys hash to the same slot. `MGET`, `MSET`, transactions with `MULTI`/`EXEC`, Lua scripts that access several keys, and `RENAME` all fail with a `CROSSSLOT` error otherwise. We found every multi-key operation by grepping the code for the relevant commands and, more reliably, by enabling a command log on a staging replica for a week and extracting every command that referenced more than one key.

### Hash tags

The standard fix is hash tags: if a key contains a substring in braces, only that substring is hashed, so `{user:42}:cart` and `{user:42}:session` always land in the same slot. We used hash tags for data that is naturally accessed together per user, such as the cart and the session, and per order, such as the order lock and its idempotency key. The trap with hash tags is that they can recreate the hot-shard problem: if you tag by something with low cardinality, like a country code or a tenant with most of the traffic, you have just built a single-shard database with extra steps. We set a rule that hash tags must contain an identifier with at least tens of thousands of distinct values in production, and the code review checklist for Redis keys now asks for the expected cardinality of the tag.

#### Rewriting the rate limiter

The rate limiter was the one place hash tags did not help. It used a Lua script that read a per-user counter and a global counter in one atomic step, so a user could be limited both individually and as part of the overall traffic to an endpoint. The two keys could never share a slot because the global counter is shared by everyone. We split the check into two independent scripts, one for the per-user counter and one for the global counter, and accepted that they are no longer checked atomically together. In the worst case a request can pass the user check and then fail the global check, having consumed one unit of the user's budget without being served. We measured how often this happens under synthetic load at about one request in fifty thousand, and decided that was acceptable for a rate limiter whose limits are approximate anyway. Example of the per-user script after the split:

```lua
local current = redis.call('INCR', KEYS[1])
if current == 1 then
  redis.call('PEXPIRE', KEYS[1], ARGV[1])
end
return current
```

### Pipelines are not transactions

A related misunderstanding showed up in code review more than once: pipelining is fine in a cluster, because a cluster client splits the pipeline by node and sends each part to the right primary. Transactions are not, because `MULTI`/`EXEC` must run on one node. Several services used `MULTI` only as a way to batch commands, not because they needed atomicity, and those were simply converted to pipelines. The ones that genuinely needed atomicity got hash tags.

## The cutover

### Sessions

Sessions were the riskiest workload because losing them logs out every user. We considered using the replication tooling to sync the old primary into the cluster, but it cannot cleanly handle keys moving between slots during sync. Instead we used dual writes: for two weeks, the session service wrote every session to both the old primary and the cluster, and read from the old primary. Sessions have a sliding expiry of thirty days, but more than ninety percent of active sessions are touched at least once a week, so after two weeks of dual writes almost every active session existed in both places. On cutover day we switched reads to the cluster with a feature flag. For the few sessions missing from the cluster, the session service fell back to reading the old primary and copying the session over on first access. The fallback ran for a further month and copied about four thousand sessions in total, after which we turned it off and decommissioned the old primary.

#### Cache

The page cache needed no migration at all: we pointed the services at the cluster during a quiet hour and let the cache warm up from scratch. Page latency at the 95th percentile rose from 120 ms to about 300 ms for the first eight minutes and then returned to normal. We had prepared a warm-up script that pre-fetched the five thousand most popular product pages, but in the end did not run it, because the rise was well inside the error budget and the script would have created its own load spike.

## Operations after the migration

Running a cluster is different from running a single primary. Resharding moves slots between nodes while they serve traffic, and while a slot is migrating, clients receive `ASK` redirects for keys that have already moved. We practised resharding in staging under synthetic load and found that our largest keys, still over 50 MB in a few places, caused latency spikes of several hundred milliseconds when they were migrated, because migrating a key is a blocking operation on the source node. We added a weekly report of keys larger than 10 MB, assigned owners, and the largest remaining key is now about 6 MB.

### Failover behaviour

In the old setup a failover took about thirty seconds because Sentinel needed to agree that the primary was down and clients needed to learn the new address. In the cluster, failover is per shard and the cluster itself promotes a replica, typically within five to ten seconds with our timeout settings. More importantly, a failover now affects one sixth of the keyspace rather than all of it. We tested this by killing a primary during peak traffic in staging and saw a short burst of errors on roughly a sixth of requests that touched Redis, recovering within eight seconds. Example of the setting that controls how quickly a primary is considered failed:

```text
cluster-node-timeout 5000
```

Setting it lower makes failover faster but increases the risk of false failovers during brief network blips, and five seconds was the value where we saw no false failovers over a month of staging chaos tests.

##### Memory fragmentation

One operational issue we had not anticipated was memory fragmentation on the cache shards. After a few weeks, the ratio of memory reported by the operating system to memory used by data reached 1.6 on two shards, meaning a third of the memory was wasted. Active defragmentation fixed it but initially used enough CPU to raise latency noticeably; we tuned it to run with lower effort and the fragmentation ratio now stays around 1.15.

## Results

Peak CPU on the busiest shard is about 45 percent, compared to a saturated single core before. Memory headroom is comfortable and adding a shard is a routine operation rather than a capacity crisis. Checkout errors during Redis failovers dropped from a visible spike to something lost in normal noise. The migration took eleven weeks from the first workload split to decommissioning the old primary, with most of the time spent on finding and fixing multi-key operations and on the two-week dual-write window for sessions.

If we did it again, we would start with the command log on a replica, not with grep. Grep found about two thirds of the multi-key operations; the command log found the rest, including several in a library we had forgotten we depended on. We would also enforce the per-key size limit from day one, since every operational problem after the cutover traced back to a handful of oversized keys.
