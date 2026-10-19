# Alerting on SLO burn rate instead of raw thresholds

Our old alerts were "p95 latency > 500 ms for 5 minutes" and "error rate > 1 %". They fired at
3 a.m. for blips nobody could act on, and stayed silent during a slow two-day degradation that
quietly ate most of the month's error budget. Switching to burn-rate alerts fixed both.

## Terms

- **SLI**: what we measure, e.g. the fraction of requests answered successfully within 300 ms.
- **SLO**: the target for the SLI over a window, e.g. 99.9 % over 30 days.
- **Error budget**: what the SLO allows to fail: 0.1 % of requests, roughly 43 minutes of full
  outage per 30 days.
- **Burn rate**: how fast we are spending the budget relative to plan. Burn rate 1 spends
  exactly the budget in 30 days; burn rate 14.4 spends 2 % of it in one hour.

## Multi-window, multi-burn-rate

| Severity | Burn rate | Long window | Short window | Budget spent when firing |
|----------|-----------|-------------|--------------|--------------------------|
| Page | 14.4 | 1 h | 5 min | 2 % |
| Page | 6 | 6 h | 30 min | 5 % |
| Ticket | 1 | 3 days | 6 h | 10 % |

Both windows must exceed the threshold. The long window gives significance, the short one
makes the alert stop quickly after recovery.

PromQL sketch for the first row (SLO 99.9 %):

```promql
(
  sum(rate(http_requests_total{job="api",code=~"5.."}[1h]))
  / sum(rate(http_requests_total{job="api"}[1h]))
) > (14.4 * 0.001)
and
(
  sum(rate(http_requests_total{job="api",code=~"5.."}[5m]))
  / sum(rate(http_requests_total{job="api"}[5m]))
) > (14.4 * 0.001)
```

## Latency SLIs and percentiles

Do not average percentiles across instances: the mean of per-pod p95 values is not the p95 of
the service. Use histograms and aggregate buckets first:

```promql
histogram_quantile(0.95, sum by (le) (rate(http_request_duration_seconds_bucket[5m])))
```

For SLOs it is even simpler to count "good" requests with a bucket boundary at the target
(`le="0.3"`) and treat slow requests like errors. Make sure a bucket boundary actually exists
at the threshold; otherwise the quantile is interpolated and can be off by a lot.

## Practical notes

- Every page links to a runbook and a dashboard showing budget remaining.
- Low-traffic services produce noisy ratios; add a minimum request count or use longer windows.
- Keep symptom-based paging (user impact) and send cause-based signals (disk 85 % full) to tickets.
