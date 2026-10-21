# Recall@k or nDCG@10: picking the metric per pipeline stage

Short rule I keep coming back to:

- **Candidate generation** (BM25, dense, hybrid feeding a reranker or an LLM context window): report recall@k where k is the number of items passed downstream. Order inside the window barely matters, missing items are fatal.
- **Final ranking** shown to a person or consumed top-first by an agent: report nDCG@10 (several relevant items, graded labels) or MRR@10 (one right answer).

A change can move them in opposite directions. Deeper fusion lists often raise recall@50 while pulling a few noisy items into the top 10, lowering nDCG@10. A reranker does the reverse: it cannot change recall@50 of its input, only the order.

When only one number fits on a dashboard, I use nDCG@10 for end-to-end search and recall@window for retrieval feeding an LLM.
