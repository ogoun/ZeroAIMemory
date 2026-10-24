---
tags: [machine-learning, serving, gpu, inference]
---
# Serving the recommendation and ranking models in production

These notes cover how the recommendation and search ranking models are served, from the moment a trained model is registered to the moment a request gets scores back. They grew out of a year of operating the serving platform and of several incidents that taught us what matters. The audience is engineers on the ML platform team and data scientists who want to understand what happens to their model after training. Training itself is out of scope. The sections on batching and on model rollout are the most practical; the section on feature skew is the one that explains most of the strange behaviour people report.

#### The models

##### What we serve

There are three model families in production. The candidate retrieval model is a two-tower network that maps users and products into the same embedding space; product embeddings are computed offline and loaded into a vector index, and only the user tower runs online. The ranking model is a gradient-boosted tree ensemble that scores a few hundred candidates per request using about two hundred features. The search reranking model is a small cross-encoder transformer that scores query and product title pairs for the top forty search results. The three have very different serving profiles: the user tower is a small neural network that needs low latency on every page view, the ranking model is CPU-bound and processes hundreds of rows per request, and the cross-encoder is the only one that needs a GPU.

###### Latency budgets

The recommendation widget on the home page has a total budget of 120 ms at the 99th percentile, of which the model calls may use about 60. Search reranking has a budget of 40 ms at the 99th percentile on top of the base search latency. These budgets were negotiated with the product teams and they drive almost every design decision below: if a model does not fit in its budget on the serving hardware, it does not ship, regardless of its offline metrics.

#### Serving architecture

##### Model server

All three families are served by the same model server, which loads models from the model registry, exposes a gRPC endpoint per model version and handles batching, metrics and health checks. Tree ensembles run on CPU nodes through a compiled inference library, and neural models are exported to ONNX and run through ONNX Runtime, on CPU for the user tower and with the CUDA execution provider for the cross-encoder. We standardized on ONNX after a period in which each model was served by its training framework's own server; that worked, but it meant three different sets of operational problems, three different metrics formats and three different ways for a model upgrade to fail. Example of the serving configuration for a model version:

```yaml
model: search-rerank
version: 2026-02-11-a
runtime: onnxruntime
device: cuda
max_batch_size: 64
batch_timeout_ms: 3
intra_op_threads: 2
warmup_requests: 200
```

###### Warm-up

The `warmup_requests` setting turned out to be essential. Without warm-up, the first requests to a newly loaded model were dramatically slower: for the cross-encoder, the first few hundred requests after loading took up to ten times longer than steady state, because the GPU runtime was compiling kernels and allocating memory pools for each new input shape. When a new pod joined the load balancer cold, its first minute of traffic violated the latency budget. The server now replays a set of recorded production requests with a representative spread of input lengths against each model before reporting itself ready, and only then does the pod receive traffic.

##### Batching

Dynamic batching is the main lever for GPU efficiency. Requests for the cross-encoder arrive individually, each with about forty query and title pairs, and the server collects requests for up to three milliseconds or until the batch reaches sixty-four pairs, whichever comes first, then runs one inference call for the whole batch. The batch timeout is a direct trade between latency and throughput. With a timeout of one millisecond, the median batch at peak traffic was about twenty pairs and GPU utilization stayed under thirty percent. With three milliseconds, the median batch reached the maximum of sixty-four at peak, GPU utilization rose to about seventy percent, and the 99th percentile latency increased by only two milliseconds, because the time saved by more efficient batches offset most of the time spent waiting. We settled on three milliseconds after measuring a range of values under replayed peak traffic.

###### Padding and sequence length

Transformers process batches of equal-length sequences, so shorter inputs are padded to the length of the longest input in the batch. Product titles vary from a few tokens to over a hundred, and a batch with one very long title wastes most of its compute on padding for all the others. We introduced length bucketing: the server keeps separate queues for inputs up to 32, 64 and 128 tokens and batches each queue separately. Titles longer than 128 tokens are truncated, which affected less than half a percent of products and had no measurable effect on ranking quality. Bucketing raised throughput per GPU by about forty percent at the same latency.

#### Feature skew

The most frequent source of confusing behaviour is training-serving skew: a feature computed one way in the offline training pipeline and a slightly different way in the online feature service. The model then sees values in production that it never saw in training, and its predictions degrade in ways that are hard to diagnose because nothing is technically broken. In our first year we found skew in about one in five new models, usually only after a disappointing online experiment. One memorable example was a feature for the number of days since the user's last purchase. Offline, it was computed from the order table with the date truncated to midnight in UTC; online, it was computed from a cached user profile with timestamps in the user's local time zone. For users in time zones far from UTC the feature was off by one for part of every day, and since the model had learned a sharp effect around the first day after a purchase, recommendations for those users flickered between two quite different sets during the day.

The latency budgets deserve a longer explanation, because they are the constraint that people outside the team understand least and argue about most. A budget of 60 milliseconds at the 99th percentile for the model calls on the home page sounds generous until you account for everything that has to happen inside it: the request has to cross the network to the model server, wait in the batching queue, have its features fetched from the online feature service, run through the model, and come back. The feature fetch alone takes 8 to 15 milliseconds at the 99th percentile, depending on how many features the model needs, and the network adds two or three more in each direction. That leaves the model itself with something like 30 milliseconds at the tail, and the tail is what matters, because a page that waits for the slowest of several model calls will feel the 99th percentile of each of them on a much larger share of page views than one percent. When the data science team proposed a ranking model with about five hundred features instead of two hundred, the offline metrics were clearly better, but the feature fetch for five hundred features took 31 milliseconds at the 99th percentile on its own, and the model could not fit in what was left. The compromise was to keep the larger model but to precompute the hundred most expensive features per user in a nightly batch job and store them alongside the user embedding, so that the online fetch for those features is a single key lookup rather than a computation. That brought the fetch back to 12 milliseconds and the model shipped a quarter later than planned. A rather different topic that kept coming up in the same discussions was reproducibility of online scores. Product managers would sometimes report that a particular user got strange recommendations, and the team would try to reproduce the scores offline with the same model version, only to get different numbers. The cause was almost always that the features had changed between the time of the request and the time of the investigation: the user had bought something, the product had sold out, or a rating had moved. Since we started logging the full feature vector for a sample of requests, the first question in any such investigation is whether the request is in the sample, and if it is, the scores can be reproduced exactly; if it is not, we can usually only say what the model would score now. We considered logging features for every request, but at our traffic that would be several terabytes per day, and the sample has been enough for every investigation so far, provided that the investigation starts within the seven days for which the feature log is kept. Coming back to latency, the team also learned that the budgets themselves need revisiting. The 60 millisecond budget for the home page was set three years ago, when the page made two model calls in sequence; since then the calls were parallelized and the page itself became faster, so in principle the models could have more time without the user noticing. We measured the effect of artificially adding latency to the recommendation widget on engagement and found no measurable change up to an extra 40 milliseconds at the median, which is now the basis for a request to raise the budget to 80 milliseconds. Whether that request is granted is a product decision, but for the first time it is being argued with data rather than with intuition on both sides. The budget conversation also changed how the data science team evaluates models before proposing them. They now run every candidate through a latency harness that replays recorded production requests against the model server on the same hardware class as production and reports the 99th percentile of the full call, including feature fetch and batching, next to the offline quality metrics. A candidate that misses the budget is either simplified, distilled into a smaller model, or sent back with a note on which features are the most expensive to fetch. The harness has saved a lot of wasted effort: in the last two quarters, three candidates were reworked before anyone spent time on an online experiment, and one of them ended up faster and better after the expensive features were replaced by cheaper precomputed ones.

##### Logging features at serving time

The structural fix is to log the exact feature values used at serving time and to train on those logs rather than recomputing features offline. The serving platform now logs the full feature vector for a sample of requests, one percent for ranking and five percent for search reranking, together with the model version and the scores, into a feature log table. New models are trained on features from that log, joined with outcomes such as clicks and purchases, so the features in training are by construction the same as in serving. The remaining risk is a new feature that has no history in the log yet; for those, a skew check compares the distribution of the offline-computed feature with the logged online values over a week and blocks registration of the model if they differ by more than a threshold.

###### Monitoring feature distributions

In production, the server publishes summary statistics for every feature of every model, such as the share of missing values, the mean and selected quantiles, every five minutes. An alert fires when a feature's distribution moves far from its trailing week. This has caught upstream breakage more than once: the most useful catch was a change in the feature service that started returning a default of zero instead of a missing value for the product rating when the reviews service timed out. Zero is a legitimate rating value for the model, so it treated those products as terribly rated; the share of zeros in that feature jumped from under one percent to about nine percent during a reviews service slowdown, and the alert fired within ten minutes. Example of a feature monitor definition:

```yaml
monitor:
  model: ranking
  feature: product_avg_rating
  stats: [missing_share, zero_share, p50, p95]
  baseline: trailing_7d
  alert_if:
    zero_share: { abs_change_gt: 0.03 }
```

#### Rollout of new model versions

##### Shadow, canary, experiment

A new model version goes through three stages. In shadow mode, the server scores every request with both the current and the new version, returns only the current version's scores, and logs both; this verifies latency and error rates under real traffic and lets us compare score distributions without affecting users. In canary mode, a small share of traffic, usually one percent, gets the new version's scores, which verifies that nothing breaks downstream. Finally, an online experiment with a proper split measures the effect on business metrics. A model can be rolled back at any stage by a configuration change that takes effect within a minute, because both versions stay loaded until the experiment concludes.

###### The incident that added shadow mode

Shadow mode was not part of the original process. It was added after a ranking model version passed offline evaluation and went straight to a canary, where it raised CPU usage on the ranking nodes by about sixty percent because it used four times as many trees as its predecessor. The canary itself was fine at one percent of traffic, but the planned ramp to fifty percent for the experiment would have exhausted the CPU on the ranking nodes during peak hours. Shadow mode now runs for at least twenty-four hours at full traffic before any canary, which surfaces resource usage at production scale before any user sees the new model.

#### Capacity and cost

GPU nodes are the most expensive part of the platform, and the cross-encoder is the only model that uses them. We run it on a pool of GPU nodes sized for peak traffic plus one node of headroom per availability zone. Off-peak, utilization drops to around fifteen percent, and we experimented with scaling the pool down at night. Scaling down works, but scaling up is slow: a new GPU node takes about four minutes to join, including image pull and model warm-up, and morning traffic rises faster than that. We now scale on a schedule that anticipates the morning ramp by thirty minutes, with reactive scaling only as a safety net.

###### CPU fallback

When the GPU pool is unavailable or saturated, search falls back to serving without reranking, which costs a few points of relevance but keeps search working. We also built a CPU fallback for the cross-encoder using a quantized int8 version of the model, which runs within the latency budget only for the top twenty results instead of forty. In an exercise where we drained all GPU nodes in one zone during peak, the fallback served that zone's traffic for twenty minutes with a relevance drop that was barely visible in online metrics, which gave us confidence that a GPU outage is an inconvenience and not an emergency.

##### Model registry hygiene

The registry holds every model version that was ever registered, with its training data reference, evaluation results, export artifacts and the serving configuration it was validated with. Early on, the registry also held about four hundred versions that had never been deployed, because every training run registered its output automatically. We changed registration to an explicit step that requires passing the offline evaluation thresholds and the skew check, and archived versions that have not been served in ninety days. The serving platform refuses to load an archived version without an explicit override, which prevents a configuration mistake from pointing production at a model trained a year ago.

#### Lessons

Most of our serving problems were not about models. They were about cold starts, about features computed in two places, about resource usage that only showed up at full scale, and about the operational differences between frameworks. Standardizing on one model server and one export format removed a whole category of problems. Logging serving-time features removed most of the skew. Shadow mode removed surprises about resource usage. What remains hard is the latency budget: each new model generation is a little bigger than the last, and every quarter we have the same negotiation between the gain in offline metrics and the milliseconds the product teams are willing to give.
