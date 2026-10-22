---
tags: [feature-flags, rollout, platform, design]
---
# Building an internal feature flag platform: design notes and lessons

Three years ago every team had its own way of turning features on and off. Some used environment variables and redeployed to flip them, some used a table in their own database, one team used a spreadsheet that a cron job exported to a JSON file in object storage, and the mobile team used the remote configuration service of their analytics vendor. When the company decided that all user-facing changes should be released behind flags with gradual rollout, it became clear that we needed one platform. These notes describe how that platform is designed, why it is designed that way, and what we got wrong in the first year. They are written for engineers who integrate with it and for anyone who wants to change it.

## Requirements

#### What a flag needs to do

The core requirement is simple to state: given a flag key and a context describing the current request, return a value quickly and consistently. The context usually contains a user identifier, the platform, the application version, the country and sometimes the customer segment. The value is usually a boolean, but about a fifth of our flags return strings or numbers, for example the name of a variant in an experiment or a timeout in milliseconds. Evaluation must be fast enough to call on every request, ideally without any network round trip, and it must be deterministic: the same user with the same context must get the same value every time, otherwise a user can see a feature appear and disappear between page loads.

##### What a flag must not do

We also wrote down what the platform must not do, which turned out to be just as important. Flag evaluation must never block a request for more than a millisecond, must never fail a request if the platform is down, and must never be used to store secrets or large configuration. The last point sounds obvious but was violated in the first month: one team stored a list of several thousand partner identifiers in a flag value, which made every evaluation of every flag in that service slower because the whole ruleset had to be parsed on each update.

## Architecture

#### Evaluation in the client

Flags are evaluated inside each service by a client library, not by a remote call. The library downloads the full ruleset for its environment at startup, keeps it in memory and receives updates through a streaming connection. If the stream breaks, the library falls back to polling every thirty seconds, and if the platform is unreachable entirely, the library keeps using the last ruleset it has. On startup without any reachable platform, the library loads a ruleset snapshot that the deploy pipeline bakes into the container image, so a service that starts during a platform outage still gets reasonable values instead of code defaults. Example of the configuration a service passes to the library:

```yaml
flags:
  environment: production
  stream_url: https://flags.internal/stream
  snapshot_path: /etc/flags/snapshot.json
  poll_interval: 30s
  stale_warning_after: 10m
```

The `stale_warning_after` setting makes the library log a warning and emit a metric when its ruleset has not been refreshed for ten minutes. That metric has caught two real problems: a network policy change that blocked the stream for one namespace, and a library version with a bug in reconnection.

##### Consistent bucketing

Gradual rollouts assign each user to a bucket by hashing the user identifier together with the flag key, so that a user who is in the first ten percent for one flag is not automatically in the first ten percent for every flag. We use a 32-bit hash of the concatenated flag key, a separator and the user identifier, mapped to one of ten thousand buckets. Increasing a rollout percentage only adds buckets, never reshuffles them, so a user who already has the feature keeps it as the rollout grows. For flags that must be consistent across related users, such as all members of a household or all employees of a business customer, the flag can be configured to bucket by a different context attribute, and the library hashes that attribute instead of the user identifier.

#### The control plane

The control plane is a small service with a database, a web interface and an API. Every change to a flag is recorded with the author, a timestamp, the old and new rule, and a free-text reason that is required for production changes. Flags have owners, and the owner is a team, not a person, because people move between teams and flags outlive their authors. Changes to production flags that affect more than half of traffic require a second approver from the owning team. We argued for a long time about whether approvals would slow everyone down; in practice most changes are small rollout steps below the threshold, and the approval requirement only applies to the big switches where a second pair of eyes is worth it.

##### Environments

Each environment has its own ruleset, and a flag exists in all environments with independent rules. Promoting a rule from staging to production is an explicit action, not a copy that happens automatically, because the most common mistake in the first months was enabling a flag fully in staging and then clicking a button that copied the staging rules into production, enabling it for everyone at once. The promote action now shows a diff of what would change in production and asks for a rollout percentage rather than copying the staging value.

## Flag lifecycle

The hardest problem with feature flags is not evaluating them, it is removing them. A flag that has been fully on for six months is not a flag any more, it is dead code with a condition in front of it, and every such flag makes the code harder to read and test. We track three kinds of flags: release flags, which hide unfinished or newly launched features and should live for weeks; experiment flags, which assign variants for an experiment and should be removed when the experiment ends; and operational flags, which are kill switches for expensive or risky functionality and may live indefinitely. Every flag must be created with a kind, and release and experiment flags must have an expiry date.

#### Enforcing removal

When a release flag passes its expiry date, the platform does three things. It opens a ticket for the owning team, it adds a warning to the flag's page, and after another thirty days it starts emitting a daily metric that feeds into the owning team's engineering health score. We tried harder enforcement in the first year: expired flags were automatically frozen, so their rules could not be changed. That backfired badly when an expired flag needed to be turned off during an incident and nobody on call could unfreeze it quickly. Freezing was removed after that incident, and we now rely on visibility rather than force. The number of expired flags across the company dropped from about 340 at the peak to under 60 after two quarters of this, mostly because teams started cleaning up during regular sprint work once the list was visible to their managers.

##### Code references

To make removal easier, a nightly job scans all repositories for references to each flag key and shows them on the flag's page, with links to the files and lines. When a flag is ready to be removed, the owner can see exactly where it is used. The scanner understands the client library's call patterns in our main languages, and for keys built dynamically from strings, which we discourage but cannot entirely prevent, it reports the flag as "references unknown" so the owner knows the list may be incomplete.

## Experiments

Experiment flags are evaluated the same way as release flags, but the platform also records an exposure event the first time a user is evaluated into a variant in a session. Exposure events go to the analytics pipeline, which computes experiment results. Early on we recorded exposure on every evaluation, which generated an enormous number of events and, worse, recorded exposure for users who never saw the feature: many services evaluate flags at the top of a request handler even when the feature is on a page the user never reaches. We changed the guidance so that experiment flags are evaluated as close as possible to where the feature is actually shown, and the library records exposure only when the calling code marks the evaluation as visible. This reduced exposure volume by about eighty percent and made experiment results noticeably less noisy, because the comparison groups now contain only users who could have been affected.

#### Sample ratio mismatch

A recurring problem in experiments was sample ratio mismatch: a test configured for a fifty-fifty split ending up with fifty-three to forty-seven, which almost always indicates a bug rather than chance. The analytics pipeline now runs a chi-square test on the exposure counts of every experiment daily and flags any experiment where the observed split is unlikely under the configured one. The most interesting cause we found was a mobile client that cached flag values on disk and kept using a stale variant after the experiment configuration changed, which skewed exposure counts toward the variant that had been assigned first.

## Performance

Flag evaluation in the library takes about two microseconds for a typical rule with three conditions, measured on our standard service instances. The full ruleset for production is currently about 2,400 flags and 1.1 MB serialized, and an update after a single flag change is delivered to all connected clients within a second at the 99th percentile. Run this to see what a particular service currently has loaded:

```sh
curl -s localhost:9090/debug/flags | jq '.ruleset_version, .last_update, .flag_count'
```

###### Ruleset size

Ruleset size matters more than evaluation speed. Every client keeps the whole ruleset in memory, and every update is a full ruleset push in the first version of the protocol. When the ruleset reached about 4 MB because of the partner-list flag mentioned earlier and a few others like it, mobile clients on slow connections began to fall behind on updates. We introduced a per-flag size limit of 16 KB and switched the streaming protocol to send per-flag deltas, with a full snapshot only on reconnect.

## Incidents involving flags

Flags are a safety mechanism, but they also cause incidents. The worst one in the first year happened when someone turned on a flag that enabled a new pricing calculation for all users, intending to enable it only for internal testers. The rule editor at the time had a percentage slider directly beside the segment selector, and the slider had been dragged to one hundred percent by accident while selecting the segment. Prices were wrong for eleven minutes until the flag was turned off. After that incident, changes that move a flag from below ten percent to above fifty percent in one step require typing the flag key as confirmation, and the rule editor shows a plain-language summary of who will be affected before saving.

#### Kill switches during incidents

On the positive side, operational kill switches have shortened many incidents. The recommendation service, the search autocomplete and the image resizing pipeline all have kill switches that replace the feature with a cheaper fallback, and on-call engineers use them several times a month. We now ask that every new feature with a meaningful cost or dependency ships with a kill switch, and the platform has a dedicated view of all operational flags so on-call engineers do not have to search for them during an incident.

## What we would do differently

If we started again, we would build the flag lifecycle tooling, expiry dates and code references, before anything else, rather than adding them a year in when hundreds of stale flags already existed. We would also have made exposure recording explicit from the start. And we would have spent less time on a sophisticated rule language: most flags use one or two conditions, and the three most complex rule features we built are used by fewer than ten flags combined, while each of them added complexity to every client library in every language.
