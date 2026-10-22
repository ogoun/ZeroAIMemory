---
tags: [observability, metrics, logging, tracing]
---
# Observability stack: design decisions and the reasoning behind them

This document explains how metrics, logs and traces are collected, stored and queried across our production systems, and, more importantly, why each part is the way it is. Most of the decisions here were contested at the time, and several were reversed after a year of operation. The intended readers are engineers who build services and want to know what they get for free, and engineers on the platform team who will one day want to change something and need to know what constraints shaped it. The sections on cardinality and on log sampling are the ones that most often surprise newcomers.

## Goals

#### What observability is for

We started from a short list of questions that an on-call engineer should be able to answer within five minutes of an alert: what is broken, since when, for whom, and what changed. Metrics answer "what" and "since when" cheaply. Traces answer "where in the call chain". Logs answer "why this specific request failed". Change events, such as deploys, flag changes and configuration pushes, answer "what changed", and they turned out to be the most underrated signal: in our incident reviews, more than half of incidents were correlated with a change in the hour before they started.

##### Non-goals

The stack is not a data warehouse. People repeatedly asked to keep request logs for a year so that product analysts could query them, and we repeatedly said no, because operational logs and analytical events have different retention, different access controls and different cost profiles. Product events go through the analytics pipeline. The observability stack keeps data for as long as it is useful for operating the system, which for most data is weeks, not years.

## Metrics

#### Collection

Services expose metrics in the Prometheus text format on a local port, and an agent on each node scrapes them every fifteen seconds and remote-writes them to the central time series database. We chose pull-based scraping on the node rather than having services push, because it means a service that is hung or dead is detected immediately as a failed scrape, while a push-based service that stops pushing just looks like a gap in the graph. The central database is a horizontally scalable Prometheus-compatible store with eighteen months of retention for downsampled data and thirty days at full resolution. Example of a typical service metric definition:

```go
var requestDuration = prometheus.NewHistogramVec(
    prometheus.HistogramOpts{
        Name:    "http_request_duration_seconds",
        Help:    "Request latency by route and status class.",
        Buckets: []float64{0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1, 2.5, 5},
    },
    []string{"route", "method", "status_class"},
)
```

##### Cardinality

The single most important rule for metrics is to keep label cardinality bounded. Every unique combination of label values is a separate time series, and the cost of the database grows with the number of active series, not with the number of samples. In the first year, one service added the customer identifier as a label to its request counter, which created about four hundred thousand new series within an hour and slowed queries for everyone until the series were dropped. We now enforce a limit of ten thousand active series per service per metric name at the agent, and series beyond that limit are dropped with a counter that alerts the owning team. The limit sounds strict, and there were complaints, but in two years only three teams have asked for an exception, and two of them found a better design after a conversation. Identifiers like customer, order or request id belong in logs and traces, not in metric labels.

#### Alerting on symptoms

Alerts are defined on service-level indicators that reflect what users experience: request error rate, latency at a percentile, and freshness for asynchronous pipelines. We discourage alerts on causes such as CPU usage or memory, because high CPU that does not affect users is not an emergency, and many causes that do affect users are not visible in resource metrics. Each service team defines objectives for their indicators, and alerts fire on the rate at which the error budget is being burned, using two windows: a fast burn over five minutes and one hour, which pages, and a slow burn over six hours and three days, which creates a ticket. The burn-rate approach replaced static thresholds and cut the number of pages per week by about sixty percent without missing any incident that users reported.

## Logs

#### Structured logging

All services log in JSON with a small set of required fields: timestamp, level, service, environment, trace id, span id and message. Everything else is free-form but must be in fields, not interpolated into the message, so that logs can be filtered by field rather than by regular expression. Logs go from stdout to the node agent, which parses them, adds metadata about the pod and node, and ships them to the log store.

##### Sampling and volume

Log volume is the biggest cost of the whole stack, larger than metrics and traces combined. At the peak before we introduced sampling, production generated about eleven terabytes of logs per day, most of it from a few chatty services logging every successful request at info level. Our first attempt to control volume was a daily quota per service with hard dropping when the quota was exceeded; it was unpopular and dangerous, because quotas tended to run out in the middle of incidents, exactly when logs were most needed. The current approach is dynamic sampling at the node agent: logs at warning and error level are always kept, and info and debug logs are sampled per service with a rate that adjusts every minute to keep each service near its budget, while logs that share a trace id with any kept error are kept too. That last rule matters a lot, because it means the full log context of a failed request is almost always available even when routine logs for that service are being heavily sampled. Daily volume is now about three terabytes, and in incident reviews nobody has reported missing logs that they needed.

#### Retention and access

Logs are kept for fourteen days in the hot store and ninety days in a cheaper archive that can be queried with a delay of a few minutes. Access to production logs is granted per team, and logs from services that handle payment or personal data go to a separate store with stricter access and shorter retention. Example query in the log store's query language, finding errors for one route in the last hour:

```text
service="checkout-api" level="error" route="/api/orders" | stats count() by error_code
```

## Traces

#### Instrumentation

Services are instrumented with the OpenTelemetry SDK, and most of the instrumentation comes from shared libraries: the HTTP server and client wrappers, the database driver wrappers and the message queue consumers create spans automatically. Teams add custom spans for significant internal operations. Trace context is propagated in the standard W3C headers, and our message queue wrappers also put the context into message headers so that asynchronous work shows up in the same trace as the request that triggered it.

##### Sampling

We sample traces at the head: the service that starts a trace decides whether to sample it, and downstream services follow that decision. The base rate is one percent, plus all traces for requests that carry a debug header, which engineers can set from a browser extension. Tail-based sampling, where the decision is made after the trace is complete so that all slow or failed traces are kept, would be better, and we ran it in a pilot for three months. It was abandoned for now because the collectors needed to buffer complete traces in memory, and for our longest asynchronous workflows a trace can take minutes to complete; the memory required for the buffer made the collector fleet about four times more expensive than head sampling with the same throughput. The compromise is that error spans are always exported even for unsampled traces, as orphan spans, which gives the error context without the full trace.

##### Trace storage and query

Traces are stored for seven days. Engineers usually find a trace in one of three ways: from a log line, which carries the trace id; from an exemplar on a latency histogram, where the metrics database stores a sample trace id alongside each histogram bucket; or by searching for traces of a service and route with a minimum duration. Exemplars turned out to be the most useful path by far, because they connect the question "why is the 99th percentile high" directly to concrete slow traces, without anyone having to guess which requests to look at. Enabling exemplars required a newer version of the metrics client library in every service, and the rollout took most of a quarter because some services were pinned to old versions for unrelated reasons. A practical note on span attributes: we limit each span to 64 attributes and each attribute value to 1 KB, and the SDK truncates anything longer. Before the limit, one service attached the entire request body to a span attribute for debugging, and a few very large requests produced spans of several megabytes each, which slowed down trace queries for everyone that touched those traces. The limit is enforced in the SDK configuration distributed through the shared library, so teams do not have to remember it.

#### Profiling

Continuous profiling is the newest part of the stack. A profiling agent samples CPU stacks of every process at a low rate and sends aggregated profiles to a central store, where they can be viewed as flame graphs per service and compared across versions. The overhead is around one percent of CPU, which we measured on the three busiest services before enabling it everywhere. Profiles have already paid for themselves several times: the most striking case was a JSON serialization library that a common internal package had switched to, which was using about fifteen percent of CPU across the fleet; reverting it saved more than the whole profiling system costs per year.

## Change events

Every deploy, feature flag change, configuration push, database migration and scaling event is recorded as an annotation in the time series database, with the service, the author and a link to the change. Dashboards show these annotations as vertical lines on every graph. This sounds trivial, and technically it is, but it is the feature on-call engineers cite most often as useful. A latency graph with a vertical line labelled with a deploy two minutes before the latency rose answers the question "what changed" faster than any amount of investigation.

#### Correlation in practice

To make annotations useful, each one carries the same service label as the metrics, so that a dashboard for a service shows its own changes and the changes of its direct dependencies, which come from the service catalogue. Showing all changes from the whole company on every graph was tried first and was useless: on a busy afternoon there are dozens of deploys an hour, and the graph becomes a barcode.

## Dashboards

Each service gets a generated dashboard with its indicators, resource usage, dependencies and recent changes, created from the service catalogue entry without anyone writing it by hand. Teams can add their own dashboards, but the generated one is the starting point for every alert, and alert notifications link to it directly. We review dashboards twice a year and delete those that nobody has opened in ninety days; the first such cleanup removed about seventy percent of all dashboards, and nobody complained.

###### Cost visibility

The stack's monthly cost is broken down by team, based on series counts, log volume and span volume attributed through the service label. Showing teams their own cost was the single most effective measure for reducing waste: within two months of publishing per-team costs, the three largest log producers reduced their volume by half on their own initiative.

## Lessons

The biggest lesson is that the hard problems in observability are social and economic, not technical. The technology for collecting metrics, logs and traces is mature. What takes effort is agreeing on what to collect, keeping cardinality and volume under control, deciding who pays, and making sure that the signals are actually used during incidents. The second lesson is to design for the incident, not for the dashboard demo: the features that matter are fast queries over recent data, a reliable link from an alert to the right dashboard, and change annotations, and almost none of that looks impressive in a demo.
