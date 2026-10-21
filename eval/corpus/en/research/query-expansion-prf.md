# Query expansion: from Rocchio and RM3 to LLM rewriting

Short queries miss documents that use different wording. Expansion adds terms before retrieval.

- **Rocchio** (vector space era): move the query vector toward the centroid of relevant documents and away from non-relevant ones. With pseudo-relevance feedback, the top-k results of the first pass are simply assumed relevant.
- **RM3**: estimate a relevance language model from the top-k documents of a first BM25 pass, take the top terms by weight, interpolate with the original query (weight around 0.5), run again. A long-standing strong baseline on TREC collections; it hurts when the first pass is bad (query drift).
- **doc2query / docT5query**: expand documents instead, by generating likely questions for each and appending them at index time. No query-time cost.
- **LLM rewriting**: ask a model for synonyms, sub-questions or a pseudo-answer (query2doc), then search with the result. Good for vague questions, risky for precise identifiers, adds latency.

Rule of thumb: expansion helps lexical retrieval much more than dense retrieval, which already handles synonyms.
