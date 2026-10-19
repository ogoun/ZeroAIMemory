# Load testing: read the tail, not the average

Lessons from load-testing the checkout API before a sales event.

- **Report p50, p95, p99 and max**, never just the mean. Our mean was 140 ms while p99 was
  2.1 s; one in a hundred customers waited two seconds, and a page making ten calls hits that tail
  far more often than 1 % of the time.
- **Open vs closed model.** Tools that run N virtual users in a loop (closed model) slow down
  their own request rate when the server slows down, hiding the problem. That is coordinated
  omission. Use a constant arrival rate (k6 `constant-arrival-rate` executor, wrk2 `-R`) so
  that latency spikes show up instead of disappearing.
- **Ramp, then hold.** Step the rate up (100, 200, 400 rps…) and hold each step a few minutes;
  the knee where p99 shoots up while throughput flattens is the real capacity.
- **Test against production-like data.** An empty database makes every query fast.
- **Watch the server side** during the run: CPU, connection pool wait time, GC pauses, queue
  depth. The client numbers tell you that it broke, server metrics tell you why.
- Do not run the load generator on the same host as the system under test.
