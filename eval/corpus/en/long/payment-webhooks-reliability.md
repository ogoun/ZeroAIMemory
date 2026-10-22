---
tags: [payments, webhooks, reliability, idempotency]
---
# Making payment webhook processing reliable

Our payment provider notifies us about payment events through webhooks: a payment succeeded, failed, was refunded, was disputed, and so on. For most of the company's history, webhook processing was a small handler in the orders service that verified the signature, looked up the order and updated its status. It worked well enough until it did not. Over one quarter we had three separate incidents in which orders were marked as paid when they were not, or stayed unpaid when the money had arrived, and in each case the cause was a different gap in how we handled webhooks. This document describes the redesign that followed: what can go wrong with webhooks, how the new pipeline handles each failure mode, and how we verify that our view of payments matches the provider's.

## Properties of webhooks we had ignored

### At-least-once, unordered delivery

The provider guarantees that every event is delivered at least once, and retries with backoff for up to three days if our endpoint does not return a success status. It does not guarantee that events arrive once, and it does not guarantee order. We knew both facts in principle. In practice, the old handler assumed that a `payment.succeeded` event for an order would never arrive after a `payment.refunded` event for the same order, and that each event would be processed exactly once. The first incident happened when a refund event arrived before the corresponding success event, because the success event's first delivery had timed out on our side and was being retried. The handler processed the refund, then processed the late success event and marked the order as paid again, and the warehouse shipped an order that had been refunded.

#### Timeouts are not failures

The second property is subtler. When our endpoint takes too long to respond, the provider treats the delivery as failed and retries, even if our handler went on to process the event successfully after the provider gave up waiting. The provider's timeout is ten seconds. The old handler did all of its work synchronously before responding: it updated the order, called the inventory service to confirm the reservation, called the notification service to send a receipt and wrote an audit record. On a slow day, when the inventory service was under load, the whole handler could take longer than ten seconds, so the provider retried an event that we had in fact processed. Combined with the lack of idempotency, this produced duplicate receipts and, in the second incident, a double release of reserved stock that let the same item be sold twice.

## The new design

### Accept fast, process later

The new endpoint does only three things before responding: it verifies the signature, it parses the event envelope enough to read the event id and type, and it inserts the raw event into an inbox table with the event id as the primary key. If the insert succeeds or fails with a duplicate key, the endpoint returns success. Everything else happens asynchronously in a worker that reads from the inbox. The endpoint now responds in under fifty milliseconds at the 99th percentile, so provider retries caused by our timeouts have essentially disappeared. Example of the inbox table:

```sql
CREATE TABLE payment_webhook_inbox (
    event_id      text PRIMARY KEY,
    event_type    text NOT NULL,
    payment_id    text NOT NULL,
    provider_created_at timestamptz NOT NULL,
    received_at   timestamptz NOT NULL DEFAULT now(),
    payload       jsonb NOT NULL,
    processed_at  timestamptz,
    attempts      int NOT NULL DEFAULT 0,
    last_error    text
);
```

#### Signature verification and replay

Signature verification uses the scheme the provider recommends: an HMAC over the timestamp and the raw body with a shared secret, compared in constant time, and a check that the timestamp is no older than five minutes to limit replay. One trap we fell into during the redesign was verifying the signature over the parsed and re-serialized JSON instead of the raw request body. Re-serialization can change whitespace and key order, and the signature then fails for perfectly valid events. The first deploy of the new endpoint rejected about two percent of events for exactly this reason, on events whose payload contained characters our JSON library escaped differently from the provider's. The fix was to capture the raw body bytes before any parsing and verify against those.

### Processing with state, not with events

The worker does not apply events blindly. For every event, it fetches the current state of the payment from the provider's API and updates our records to match that state, rather than applying the change described in the event. In other words, the webhook is treated as a notification that something changed, not as the source of truth about what the state is now. This handles out-of-order delivery for free: if a late success event arrives after a refund, the worker fetches the payment, sees that it is refunded, and does nothing to mark the order as paid. It also handles missed events, because the next event for that payment brings our state up to date regardless of what happened in between. The cost is one extra API call per event to the provider, which at our volume of about four hundred thousand webhook events per day is well within the provider's rate limits.

#### Per-payment serialization

Two events for the same payment can still be processed concurrently by different worker instances, and both can fetch the provider state at slightly different moments and write conflicting updates. To prevent that, the worker takes a lock per payment id for the duration of processing. We use an advisory lock in the orders database keyed by a hash of the payment id, which is cheap and released automatically if the worker crashes. Example of the locking step:

```sql
SELECT pg_try_advisory_xact_lock(hashtext($1));
```

If the lock is already held, the worker leaves the event in the inbox for the next pass rather than waiting, which keeps worker threads from piling up behind a single busy payment.

## Side effects

Updating the order status is only part of processing. Most of the incidents came from side effects: sending receipts, releasing or confirming stock, notifying the warehouse, crediting loyalty points. In the new design, side effects are not performed directly by the webhook worker. Instead, when the worker changes the order's payment state, it writes outbox records in the same transaction, one per side effect, and separate consumers process the outbox. Each consumer is idempotent with respect to the outbox record id: the notification service records which outbox ids it has already sent receipts for, the inventory service records which ids it has already applied, and so on. If a consumer crashes after doing its work but before marking the outbox record as done, it will see the record again and skip it because the id is already recorded.

### Why not just make everything idempotent

During design review, someone asked whether the inbox and outbox were necessary at all if every operation were made idempotent. The answer is that idempotency handles duplicates but not partial failure. Without the outbox, a worker could update the order status, then crash before calling the inventory service, and nothing would ever retry the inventory call because the webhook had already been marked as processed. The outbox makes the status change and the intention to perform side effects atomic, and idempotency then makes it safe to perform side effects more than once.

## Reconciliation

Even with all of this, we do not trust webhooks alone. A reconciliation job runs every hour and compares payment states in our database with the provider's records for all payments created or updated in the last forty-eight hours. Discrepancies are fixed automatically by the same state-fetching code the worker uses, and each one is logged with the reason category. Run the reconciliation for a specific window by hand with:

```sh
payments-reconcile --since 2026-03-01T00:00:00Z --until 2026-03-02T00:00:00Z --dry-run
```

In the first month after launch, reconciliation found and fixed about thirty discrepancies per day. Nearly all of them came from a single cause: events for payments created through a legacy checkout flow that registered a different webhook endpoint with the provider, one that had been decommissioned years ago and silently returned errors. The provider had been retrying and eventually giving up on those events for as long as anyone could remember. Once that endpoint was removed from the provider's configuration and the legacy flow pointed at the new endpoint, daily discrepancies dropped to between zero and three.

#### Daily settlement report

On top of hourly reconciliation, a daily job compares the provider's settlement report, the list of money actually moved, with our records of paid and refunded orders. This catches a different class of problem: cases where both our database and the provider's payment object agree, but the money did not move as expected, for example a refund that the provider accepted but later failed at the bank. These are rare, a few per month, and go to the finance team as a report rather than being fixed automatically.

## Testing the pipeline

Testing webhook handling is awkward because the interesting cases are about timing and failure, which unit tests do not exercise well. We built a small simulator that plays the provider's role in integration tests: it sends events to the endpoint with configurable duplication, reordering and delays, serves a fake version of the provider's payment API whose state the test controls, and can make that API slow or return errors. Every scenario from the three original incidents is a test in the suite. The most valuable test is a randomized one: it creates a few hundred payments with random sequences of state changes, delivers their events with random duplication and reordering, crashes and restarts worker instances at random points, and finally checks that every order's payment state matches the fake provider and that every side effect happened exactly once from the point of view of its consumer.

### What the randomized test found

The randomized test found two bugs before launch that none of the hand-written tests had caught. The first was in the outbox consumer for loyalty points, which recorded the outbox id as processed in a separate transaction after crediting the points, so a crash between the two steps credited points twice on retry. The fix was to credit points and record the outbox id in the same transaction. The second bug was in the inbox worker: when the provider's API returned a payment that did not exist yet, which can happen for a few seconds after creation because the provider's read replicas lag behind, the worker treated it as a permanent error and moved the event to the dead-letter state. It now treats a missing payment as a transient error for the first ten minutes after the event's creation time and retries with backoff.

#### Load testing

We also load-tested the endpoint at ten times our peak event rate. The accept-fast design made this easy: the endpoint's only dependency during the request is the inbox insert, and the database handled the insert rate without visible strain. The worker side scales horizontally, and the per-payment lock meant that adding workers never caused conflicting writes, only more lock contention on the rare payments that receive many events in quick succession, such as disputed payments with multiple evidence submissions.

## Monitoring

The pipeline exposes a small set of metrics: age of the oldest unprocessed inbox event, inbox events processed per minute by type, processing errors by category, outbox lag per consumer, and reconciliation discrepancies per run. The age of the oldest unprocessed event is the primary alert; it pages if it exceeds five minutes. We deliberately do not alert on individual processing errors, because transient errors from the provider's API are expected and retried, and paging on them would create noise. Instead, events that fail processing ten times are moved to a dead-letter state and create a ticket for the payments team, with the event id, type and last error.

##### Replaying events

When a bug in processing is fixed, affected events can be replayed by resetting their processed state in the inbox. Because processing fetches current state from the provider rather than applying event contents, replaying old events is safe: it simply brings our records in line with the provider's current view. We replayed about twelve thousand events after fixing a bug in the handling of partial refunds, and the replay changed the state of exactly the two hundred and eleven orders that the bug had affected and left every other order untouched.

## Results

Since the redesign went live seven months ago, there has been no incident caused by webhook processing. The reconciliation job still finds occasional discrepancies, mostly caused by provider-side delays rather than by our processing, and fixes them within the hour. The three incidents that motivated the work each mapped to one property of webhooks that the old handler ignored: unordered delivery, retries after timeouts, and partial failure of side effects. Each property now has an explicit mechanism in the design, and we review new payment features against the same list.
