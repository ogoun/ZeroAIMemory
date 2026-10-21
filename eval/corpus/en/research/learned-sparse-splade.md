---
tags: [sparse, splade, lexical-weights]
---
# Learned sparse retrieval: SPLADE and bge-m3 lexical weights

Reading notes on models that output sparse term weights instead of (or next to) a dense vector.

## SPLADE

SPLADE (Formal et al., 2021; v2 and later follow-ups) reuses the masked-language-model head of a BERT-like encoder. For every input position the MLM head yields logits over the whole vocabulary (~30k for BERT). Those are turned into a single vocabulary-sized vector:

```
w_j = max_i log(1 + ReLU(logit_ij))
```

(the first version used a sum over positions, later versions switched to max pooling). Two properties matter:

- **Expansion.** Terms that never appear in the text can get non-zero weight ("car" activates "vehicle"). This is what closes part of the vocabulary-mismatch gap that BM25 suffers from.
- **Sparsity is trained, not given.** A FLOPS regularizer penalizes the expected number of shared non-zero terms between queries and documents, keeping posting lists short. Without it the vectors get dense and the inverted index becomes useless.

Scoring is a dot product over shared terms, so a standard inverted index works. On BEIR, SPLADE variants were competitive with or better than the dense models of the same period, with better out-of-domain robustness.

## bge-m3 sparse head

bge-m3 (BAAI, 2024) produces three outputs from one XLM-RoBERTa backbone: a dense vector, multi-vector (ColBERT-style) token embeddings, and lexical weights. The lexical part is simpler than SPLADE:

- a linear layer plus ReLU maps each token's last hidden state to one scalar weight;
- only tokens actually present in the input get a weight (no expansion); repeated tokens keep the maximum;
- the score is the sum over shared tokens of `w_q(t) · w_d(t)`.

So it behaves like a learned, context-aware term-weighting BM25 over the multilingual sentencepiece vocabulary (~250k entries). Weights for stop-like pieces end up close to zero.

## Practical notes

- Tokens are subword pieces, not words. A query term split into pieces matches documents through each piece separately; this helps with morphology but adds noise on short pieces.
- Sparse weights are not on the same scale as BM25 or cosine scores; fuse by rank or normalize.
- Storage: tens to a few hundred non-zero entries per passage, much smaller than a 1024-dim float vector.

## Open questions

- Does a learned sparse list add anything on top of BM25 + dense in a hybrid, or is it mostly redundant with one of them?
- How badly do these weights degrade on code identifiers that the tokenizer shreds into many pieces?
