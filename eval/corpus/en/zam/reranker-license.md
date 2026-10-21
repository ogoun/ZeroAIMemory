# Reranker models and the CC-BY-NC catch

Server-side reranking is opt-in per request (`"rerank": "on"`, off by default, never used in
hooks). The top 20 fused candidates (`ml.rerank_top`) are rescored with 256-token passages
(`ml.rerank_tokens`). The model follows the embedder's device and loads lazily:

- CPU: `ml.rerank_model_cpu = "jina-reranker-v2"` (jina-reranker-v2-base-multilingual, ~297 MB);
- CUDA: `ml.rerank_model_gpu = "bge-reranker-v2-m3"` (Apache-2.0).

**jina-reranker-v2 is CC-BY-NC-4.0 — non-commercial use only.** The owner kept it as the CPU
default, but it is excluded from the `zam-models` archives and downloads only with explicit
consent:

```
zam-server models fetch --device cpu --reranker --accept-nc-license
```

Without the flag the command explains the license and downloads nothing. For commercial
deployments set `ml.rerank_model_cpu = "bge-reranker-v2-m3"` and fetch it with
`zam-server models fetch --id bge-reranker-v2-m3`. It is noticeably slower on CPU: about 8 s
per 20 candidates versus 1.6 s for jina on 12 cores (0.08 s on an RTX 3090).

If no reranker is installed, search still answers, just unreranked, with
`warnings: ["reranker not installed"]`.
