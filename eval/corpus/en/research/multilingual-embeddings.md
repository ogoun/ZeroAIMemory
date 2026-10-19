# Multilingual embeddings and cross-lingual retrieval

## Setting

A Russian query should find an English passage with the same meaning, and vice versa. Two routes: translate (query or documents) and use a monolingual model, or use a multilingual encoder that maps both languages into one space.

## Models I looked at

| Model | Backbone | Dim | Max tokens | Notes |
|---|---|---|---|---|
| LaBSE | BERT, 109 languages | 768 | 512 | trained for translation pair mining; good at bitext, weaker at question–passage retrieval |
| multilingual-e5 (small/base/large) | XLM-R / MiniLM | 384–1024 | 512 | needs `query: ` / `passage: ` prefixes |
| bge-m3 | XLM-R large | 1024 | 8192 | dense + sparse + multi-vector outputs |

Benchmarks to look at: MIRACL (monolingual retrieval in 18 languages), Mr. TyDi, and the multilingual parts of MTEB. Cross-lingual numbers are reported less often than monolingual ones.

## Observations

- **Language bias.** With a mixed-language corpus, multilingual models tend to rank same-language passages higher even when a different-language passage is the better answer. The gap is smaller in newer models but still visible.
- **Lexical search does not cross languages.** BM25 contributes nothing to a Russian query against English text, except for shared identifiers and Latin-script terms. In a hybrid, the dense side must carry cross-lingual queries alone, so fusion weights tuned on monolingual queries may underweight it.
- **Tokenizer fertility.** Cyrillic text often splits into more subword pieces than English text of the same meaning, so the 512-token limit is hit sooner.
- **Mixed text.** Technical Russian notes are full of English terms. This actually helps cross-lingual matching, since the shared terms anchor both sides.

## Translation as an alternative

Translating the query into the corpus language (or both) with an MT model is a strong baseline, sometimes better than direct cross-lingual retrieval, but it adds latency and a dependency and fails on mixed-language corpora where the "corpus language" is not defined.

## Open questions

- How to measure the language bias directly: same fact written in two languages, compare ranks.
- Does adding the translated title to each chunk reduce the bias cheaply?
