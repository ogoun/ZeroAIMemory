---
tags: [api, versioning, compatibility, guide]
---
# Public API versioning and deprecation: the guide we wish we had earlier

Our public API is used by about nine hundred partner integrations, ranging from large marketplaces with dedicated engineering teams to small shops running a plugin that someone wrote years ago and nobody maintains. That spread defines almost everything in this guide. A change that a large partner adapts to in a week can break a small integration permanently, because nobody on the other side will ever read our changelog. This document describes how we version the API, what counts as a breaking change, how we deprecate and remove things, and what we learned from the two times we got it badly wrong.

## Versioning model

#### Dated versions

The API uses dated versions, such as `2025-03-01`, passed in a request header. Each partner account has a default version, pinned to the version that was current when the account first made a request, and a request can override it with the header. When we release a new version, nothing changes for existing partners until they opt in by changing their default or sending the header. Inside our code, each version is a set of transformations applied to requests and responses on top of the latest internal representation, so the core business logic only ever deals with the newest shape of the data. Example of a request that explicitly selects a version:

```sh
curl https://api.example.com/v1/orders/ord_123 \
  -H "Authorization: Bearer $TOKEN" \
  -H "Api-Version: 2025-03-01"
```

We chose dated versions over a major version in the URL after living with `/v1/` and `/v2/` for several years. The URL scheme encouraged big-bang releases, where a new major version bundled dozens of changes and partners had to migrate everything at once, so most of them never migrated. With dated versions, each version contains only a few changes, and a partner who moves forward one version at a time has a small, well-documented diff to handle each step.

##### Version transformations

Each version is described by a list of changes, and each change is implemented as a pair of functions: one that transforms an incoming request from the old shape to the new one, and one that transforms an outgoing response from the new shape to the old one. When a partner pinned to an old version makes a request, the request passes through every transformation from their version up to the latest, the handler runs, and the response passes back down through the same transformations in reverse. A change that renames a field is two lines; a change that splits one field into two is a few more. This layering has a cost: a partner pinned to a version from three years ago passes through about forty transformations, which adds roughly two milliseconds per request. We consider that acceptable.

## What counts as a breaking change

The most important part of the guide is the list of changes that require a new version. We publish it to partners, and internally it is enforced by a contract test suite that runs against every pull request touching the API layer. Breaking changes include removing or renaming a field, an endpoint or an enum value; changing the type or format of a field; making an optional request field required; changing the meaning of an existing field; and changing error codes for existing error conditions.

#### Changes that are not breaking, officially

Adding a new optional request field, a new response field, a new endpoint or a new event type is not breaking according to our published policy, and partners are told that their integrations must tolerate unknown fields. In practice, this is where most real breakage happens. The first incident that taught us this was adding a value to the order status enum: we added `partially_refunded`, which was non-breaking by our own rules, and a widely used open-source plugin for one shop platform crashed on any order with that status because it mapped statuses with an exhaustive switch and threw on unknown values. About a hundred and forty shops using that plugin stopped syncing orders until the plugin author released a fix nine days later. Since then, new enum values in responses are themselves versioned: partners on older versions receive the closest older value, in this case `refunded` with an additional field, and only partners on the new version see the new value.

##### Ordering, pagination and defaults

Other changes that are technically compatible but break integrations in practice include changing the default sort order of a list endpoint, changing the default page size, and changing which fields are included by default when a response supports expansion. We now treat all of these as breaking changes that require a new version, even though the published policy does not strictly require it. A partner who relied on the default sort order without specifying one is wrong according to our documentation, but the partner's customers do not care whose fault it is.

## Deprecation

#### Timeline

When a version or an endpoint is deprecated, we announce it with at least twelve months' notice for versions and six months for individual endpoints in older versions. Deprecated responses carry a `Deprecation` header with the date and a `Sunset` header with the removal date, following the relevant standards, plus a link to the migration guide. The partner dashboard shows a list of deprecated features the partner has used in the last thirty days, with request counts and example request ids. Example of the headers on a deprecated response:

```text
Deprecation: @1735689600
Sunset: Wed, 01 Jul 2026 00:00:00 GMT
Link: <https://docs.example.com/migrations/2025-03-01>; rel="deprecation"
```

##### Reaching the partners who do not read

Headers and dashboards reach the partners who look at them. A large share of partners never do. For those, we rely on usage data: three months before a sunset date, every partner account that still used the deprecated feature in the last thirty days gets an email to the technical contact on file and to the account owner. One month before, partners with significant volume get a call from their account manager. Two weeks before, we run brownouts: the deprecated feature returns an error for one hour at a scheduled time, announced in advance, so that integrations that nobody monitors produce failures that somebody notices. Brownouts were controversial internally because they deliberately break integrations, but they are the only mechanism that reliably reaches unmaintained integrations before the permanent removal. In our last major removal, the first brownout cut the remaining traffic on the deprecated endpoint by about half within a week, because the brownout triggered support tickets at the shops that used those integrations, and the shops then contacted whoever built them.

Partners often ask us how we decide that a change deserves a new version at all, given that each version adds a transformation layer we have to carry for years, and the honest answer is that the decision is a negotiation between three groups with different interests. Product teams want to ship improvements quickly and would prefer to change the existing shape of the API in place, because a new version means that only partners who opt in benefit from the improvement. The platform team that maintains the versioning layer wants as few versions as possible, because every version multiplies the combinations that the contract tests must cover. And the partner success team, which talks to partners every day, wants nothing to change for partners who did not ask for anything, because every change, however compatible on paper, produces support tickets. In practice we batch small changes into one dated version per month at most, and a change that cannot wait for the next monthly version needs an explicit exception from the API review group, which has granted four exceptions in the last two years, all for security fixes. A quite different matter that partners raise just as often is rate limiting, which is not versioned at all and has caused at least as much friction as any breaking change. Rate limits are applied per partner account and per endpoint group, and when we lowered the limit on the order search endpoint after a single partner's polling loop degraded the service for everyone, several other partners who polled less aggressively but in large bursts at the start of each hour hit the new limit too. They experienced it as a breaking change, even though no field or behaviour of the API had changed. Since then, any change to rate limits that lowers the effective limit for any partner by more than ten percent goes through the same announcement process as a deprecation, with at least thirty days of notice, a dashboard warning showing how close the partner's current traffic is to the new limit, and response headers that report the remaining budget so that well-behaved clients can slow down before they are throttled. We also added a separate, more generous limit for partners who use webhooks instead of polling, which turned out to be the most effective way to get partners to stop polling at all. Coming back to versions, one more consideration is how long we keep them. We have never removed an entire dated version, only individual features within old versions, because removing a version would force every partner pinned to it to migrate at once, recreating the big-bang upgrade problem that dated versions were meant to solve. The cost of keeping old versions has so far been small, but it is not zero, and we review it once a year. The review looks at the number of partners and the share of traffic on each version, the number of transformations that exist only to support versions older than three years, and the number of support tickets attributed to old versions. If that review ever shows that a version is used only by a handful of unmaintained integrations, the plan is to contact those partners individually and offer help with migration before announcing a removal date, but so far every old version still has at least one active, maintained integration behind it. A related question is how partners find out which version they are actually using, which is less obvious than it sounds. Many partners do not set the version header and rely on the account default, and some integrations were built by contractors who have long since left, so nobody on the partner side knows what the default is or why. The partner dashboard now shows, for the last thirty days, the versions that the account's requests actually used, broken down by API key, together with the share of requests that would change behaviour if the account default were moved to the latest version. That last number turned out to be the most persuasive argument for upgrading: when partners see that ninety-eight percent of their requests would behave identically on the latest version, and the remaining two percent are listed with example request ids, the upgrade stops looking like a project and starts looking like an afternoon of work. Partners who upgrade this way rarely come back with problems, because the list shows them exactly what to test.

#### Removal

On the sunset date, the deprecated feature starts returning a `410 Gone` error with a message pointing to the migration guide. We keep the old code path in place for a further thirty days behind a per-partner override, so that a partner with a genuine emergency can be given a short extension by support. In the last removal, fourteen partners received an extension, and all of them migrated within the thirty days.

## SDKs and generated clients

We publish official client libraries in four languages, generated from the API description and then lightly hand-edited. Each SDK release targets one API version, and upgrading the SDK is the most common way partners move to a new version, often without realizing it. That makes SDK releases part of the versioning story. A major SDK version bump is required whenever the underlying API version changes, so that partners pinned to an SDK major version are also pinned to an API version, and SDK changelogs repeat the relevant API changes in the SDK's own terms, because most partners read the changelog of the package they install, not ours.

#### Generated code and unknown values

The generated clients were the second source of enum trouble. Early versions of the generator produced strict enum types that failed to deserialize unknown values, which made any new enum value a breaking change for anyone using the official SDK, contradicting our own policy that unknown values must be tolerated. We changed the generator so that every enum type has an explicit unknown variant that carries the raw string, and every SDK's tests include a case that deserializes a response with an invented enum value. Partners who generate their own clients from the API description get a warning in the description itself: each enum is marked as extensible, and the documentation explains what that means for generated code.

##### Pagination helpers

The SDKs also hide pagination behind iterators, which turned out to have a subtle interaction with versioning. When we changed cursor encoding in one version, partners using the SDK iterators upgraded seamlessly, while partners who had copied cursors into their own databases to resume long synchronisations found that cursors from the old version were rejected by the new one. We now treat cursor format as part of the versioned contract: cursors carry the version they were issued under, and the API accepts cursors from any supported version for their full validity period, translating them internally. This was not part of the original design, and it is a good example of something partners store that we did not think of as stored data.

## The second incident: the webhook signature change

The worst incident related to versioning was not about the request API at all, but about webhooks. We changed the webhook signature scheme from an HMAC over the body to an HMAC over the timestamp and body, to protect against replay attacks. The change was announced as part of a new version, and partners on older versions continued to receive the old signature header. What we missed was that webhook delivery used the partner's default version, while some partners had pinned their requests to a newer version using the header but never changed their default. Their integration code had been updated for the new version, including the new signature verification, but their webhooks still arrived with the old signature, so verification failed on every webhook. For two days, about thirty partners rejected all webhooks, and our retry queue grew to several million events before we noticed the pattern. The fix was to let partners set the webhook version independently in the dashboard and to send both signature headers during a transition period, so that verification code written for either scheme works.

## Internal practices

#### Review checklist

Every pull request that changes the public API layer must answer a short set of questions in its description: does it add, remove or change any field, endpoint, enum value or default; if so, is the change gated behind a new version; is there a transformation for older versions; and is the changelog entry written in terms a partner understands. The API review group, a rotating set of four engineers, approves every such pull request, and the contract test suite fails the build if the response shape for any existing version changes without a corresponding version entry.

##### Contract tests

The contract test suite stores example requests and responses for every endpoint in every supported version and replays them on each build. A difference in any response for any version fails the build, unless the difference is listed as expected in the version's change list. This catches accidental breaking changes, such as a serializer upgrade that started omitting null fields, which happened once and would have broken partners who checked for the presence of a field rather than its value. Run the suite locally for one endpoint with:

```sh
make api-contract ENDPOINT=orders.retrieve VERSIONS=all
```

###### Measuring version adoption

We track how many partners and how much traffic are on each version. Today about sixty percent of traffic comes from the latest two versions, but the long tail is long: there is still meaningful traffic on versions more than three years old. We do not force partners to upgrade unless we are removing something, because every forced upgrade costs partners effort and costs us goodwill. The cost of carrying old versions is mainly in the transformation layer and the contract tests, and so far it has stayed manageable.

## Summary

Version small and often, treat "technically compatible" changes with suspicion, measure who uses what before removing anything, and accept that some partners will only notice a deprecation when it breaks. Most of the effort in API versioning is not in the versioning mechanism itself, which is a few hundred lines of transformation code, but in communication, usage tracking and the discipline of the review checklist.
