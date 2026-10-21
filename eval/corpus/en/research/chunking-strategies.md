---
tags: [chunking, rag, retrieval]
---
# Chunking strategies for retrieval: a comparison

Notes from reading RAG engineering write-ups, a few papers, and my own small experiments on technical notes. The question: how to split documents into retrievable units so that a query hits the right passage and the passage is self-contained enough to be useful.

## Why chunking matters

Embedding models have a context limit (512 tokens for many, 8192 for some newer ones), and even long-context encoders compress a long text into one vector, so a specific fact gets diluted. Lexical search suffers too: BM25 on a 10-page document rewards it for matching words scattered across unrelated sections. Chunks are the compromise between **precision** (small, focused units) and **context** (enough surrounding text to be understood).

## Strategies

### Fixed-size windows

Split every N tokens with an overlap of M tokens (for example 400 with 50 overlap).

- Pros: trivial, predictable sizes, works for any text.
- Cons: cuts through sentences, code blocks and tables; a heading may land in one chunk and its content in the next. Overlap duplicates text and inflates the index by M/N.

### Recursive splitting by separators

Try the coarsest separator first (blank lines between sections), and only if a piece is still too big, split it by the next one (paragraphs, then sentences, then words). This is what most RAG frameworks default to.

- Pros: respects natural boundaries most of the time.
- Cons: sizes vary a lot; tiny fragments appear and need to be merged with neighbours.

### Structure-aware splitting

Use the document's own structure: Markdown headings, HTML sections, function boundaries in code. Each chunk is a section (or a run of small sections), and the heading path is carried along.

- Pros: chunks match how the author organized the content; the heading path is a strong retrieval signal.
- Cons: needs a parser per format; very long sections still need a fallback splitter; very short sections need merging.

### Sentence and semantic splitting

Embed every sentence and start a new chunk where the similarity between neighbouring sentences drops below a threshold (or a percentile of all drops).

- Pros: topic shifts become boundaries even without headings.
- Cons: an embedding call per sentence at index time; thresholds are corpus-dependent; published comparisons show mixed gains over simple recursive splitting, sometimes none.

### Late chunking

Run a long-context encoder over the whole document first, then pool token embeddings per chunk span (Günther et al., Jina AI, 2024). Each chunk vector is conditioned on the full document, so pronouns and implicit references keep their meaning.

- Pros: context without extra text or LLM calls.
- Cons: requires a long-context model and access to token-level outputs; documents longer than the model window still need macro-splitting.

### Small-to-big (parent/child)

Index small chunks for matching, but return the larger parent section to the reader or LLM.

- Pros: precise matching plus enough context for the answer.
- Cons: two levels to maintain; the parent can blow the context budget.

## Comparison

| Strategy | Index cost | Boundary quality | Context per chunk | Typical use |
|---|---|---|---|---|
| Fixed windows | lowest | poor | low | baseline, unstructured text |
| Recursive | low | good | medium | default for prose |
| Structure-aware | low | very good | medium–high | Markdown, docs, code |
| Semantic | high | good | medium | long unstructured prose |
| Late chunking | medium | depends on base split | high | long documents, references |
| Small-to-big | medium | good | high at read time | QA over manuals |

## Size

The sweet spot reported again and again is roughly 200–500 tokens per chunk for passage retrieval, with smaller chunks favouring precise factual lookups and larger ones favouring explanatory questions. This is strongly corpus- and query-dependent; the only reliable approach is to sweep sizes on a labelled query set.

A simple sweep that worked for me:

```text
for target in 200 300 400 600 800:
    rebuild index with chunk target size = target
    run labelled queries
    record recall@5, MRR@10, nDCG@10 (document level)
pick the smallest size within the confidence interval of the best
```

Measure at the **document** level if the user ultimately wants documents: otherwise smaller chunks look better simply because a document is represented by more chunks.

## Practical checklist

- Never split inside a fenced code block or a table row; keep the block whole or split it on line boundaries with the header repeated.
- Prepend the document title and heading path to each chunk before embedding. It is a cheap form of contextualization and helps both lexical and dense retrieval.
- Merge tiny chunks (under ~50 tokens) into a neighbour; they produce noisy embeddings.
- Deduplicate results by document after retrieval, keeping the best chunk.
- Store chunk offsets so that the reader can show the surrounding text.
- Re-chunking changes every vector: version the chunking parameters and rebuild the index when they change.

## Failure modes to look for when inspecting results

- A chunk that starts with "This option…" or "In that case…" and is retrieved for the wrong question because its subject lives in the previous chunk.
- A list split in half: the query matches the second half, which lacks the introduction that explains what the list enumerates.
- A table separated from its caption or header row, so the numbers lose their meaning.
- Boilerplate (licence headers, navigation, front matter) forming its own chunks and matching generic queries.

Reading twenty failed queries by hand usually reveals which of these dominates, faster than any automated sweep.

## Open questions

- How much of the benefit of semantic splitting disappears once headings are used as boundaries?
- Is overlap worth it at all when chunks follow structure? My small runs show no measurable difference, but the query set was tiny.
- Does late chunking help multilingual documents where the referent and the reference are in different languages?
