# Contextual retrieval: situating each passage before indexing

Anthropic described the technique in September 2024. Before indexing, an LLM receives the whole source document plus one passage and writes a short blurb (roughly 50–100 tokens) explaining where the passage sits: which company, which quarter, which section. The blurb is prepended to the passage, and both the embedding and the BM25 index are built over the combined text.

Reported numbers (their own benchmark, so treat as indicative): failures in the top-20 dropped by about 35% with contextualized embeddings, about 49% when BM25 was contextualized as well, and about 67% with a reranker on top.

Cost: one LLM call per passage at ingest time. Prompt caching of the full document makes this affordable because the document prefix is reused for all its passages.

A cheaper approximation without an LLM: prepend the document title and heading path. It fixes the most common failure, a passage that says "the limit is 30 seconds" without naming what is limited.
