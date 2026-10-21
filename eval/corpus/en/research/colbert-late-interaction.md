# Late interaction: ColBERT and MaxSim

## Idea

Single-vector bi-encoders squeeze a passage into one embedding; cross-encoders attend over the query and passage jointly but cannot precompute anything. ColBERT (Khattab and Zaharia, 2020) sits in between: both sides are encoded independently, but every token keeps its own vector, and the interaction happens late, at scoring time:

```
score(q, d) = Σ_{i ∈ query tokens} max_{j ∈ doc tokens} ( q_i · d_j )
```

Each query token finds its best-matching document token (MaxSim), and the maxima are summed. Vectors are L2-normalized and projected down to small dimensions (128 in the original).

## Why it works

Token-level matching keeps fine-grained evidence that pooling throws away: a rare term in the query can match its exact counterpart in the passage even if the rest of the passage is about something else. Empirically ColBERT-style models are close to cross-encoder quality on MS MARCO and generalize better out of domain than single-vector models of similar size.

## Cost

Storage is the main issue: one vector per token instead of one per passage.

| Representation | Per 300-token passage |
|---|---|
| Single dense, 1024 × fp32 | 4 KB |
| ColBERT, 300 × 128 × fp16 | ~75 KB |
| ColBERTv2 residual compression (1–2 bits per dim) | ~6–12 KB |

ColBERTv2 (Santhanam et al., 2022) stores each token vector as a centroid id plus a quantized residual; PLAID adds centroid-based candidate pruning to make search fast. Without those tricks, late interaction is mostly used as a **reranker** over candidates from a cheaper first stage: compute MaxSim on 100 candidates, no special index needed.

bge-m3 also exposes multi-vector output usable the same way.

## Open questions

- For short notes (a few hundred tokens), is MaxSim reranking a cheaper substitute for a cross-encoder at similar quality?
- How much does query augmentation with [MASK] tokens matter outside the original ColBERT training setup?
