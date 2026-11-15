# Итоговый замер M6 — 2026-11-15 (bge-m3-fp16)

Индекс файлов сравнений `cargo xtask eval` и значения критериев предрегистрации задачи 13b (журнал этапа). Своей статистики нет: числа — из файлов ниже.

## ТС-2, агент хуков claude (попытка 2)

- R полный M6/B5.6, standard: 1.087 [1.043–1.143] (порог точечной ≤ 1.15)
- R без Rules M6/B5.7, standard: 0.973 [0.963–0.983] (порог ≤ 1.0)
- R полный M6/B5.6, всего: 1.088 [0.983–1.215]
- Δ hint_recall: 0.000 [0.000–0.000]; строго: 0.000 [0.000–0.000]
- обязательные строки M6: {'by_profile': {'standard': {'found': 87, 'total': 87}, 'heavy': {'found': 165, 'total': 165}}, 'worst_rep_share': 1.0}
- подсказки-идеи M6: {'found': 21, 'total': 21}; совпадение с B5.7: {'found': 24, 'total': 24}
- промахи: {'m6': {'reps_with_misses': [], 'hook_budget_misses': 0, 'bad_outputs': 0}, 'b56': {'reps_with_misses': [], 'hook_budget_misses': 0, 'bad_outputs': 0}, 'b57': {'reps_with_misses': [], 'hook_budget_misses': 0, 'bad_outputs': 0}, 'aa': {'reps_with_misses': ['sh-02 r1', 'sh-03 r2'], 'hook_budget_misses': 2, 'bad_outputs': 0}}
- порог шума A/A (p95 |Δ|/база): 0.0078047119434925165
- проверки: {'r_full_standard_le_1_15': True, 'r_norules_standard_vs_b57_le_1_00': True, 'required_1_0': True, 'd_hint_recall_lo_ge_-0_02': True, 'd_hint_recall_strict_lo_ge_-0_02': True, 'idea_hint_recall_1_0': True, 'idea_hints_match_b57': True, 'no_reps_with_misses': False, 'same_utc_day': True}

## ТС-2, агент хуков codex (попытка 1)

- R полный M6/B5.6, standard: 1.088 [1.043–1.144] (порог точечной ≤ 1.15)
- R без Rules M6/B5.7, standard: 0.974 [0.964–0.983] (порог ≤ 1.0)
- R полный M6/B5.6, всего: 1.059 [0.987–1.141]
- Δ hint_recall: 0.000 [0.000–0.000]; строго: 0.000 [0.000–0.000]
- обязательные строки M6: {'by_profile': {'standard': {'found': 87, 'total': 87}, 'heavy': {'found': 162, 'total': 165}}, 'worst_rep_share': 0.9880952380952381}
- подсказки-идеи M6: {'found': 21, 'total': 21}; совпадение с B5.7: {'found': 24, 'total': 24}
- промахи: {'m6': {'reps_with_misses': [], 'hook_budget_misses': 0, 'bad_outputs': 0}, 'b56': {'reps_with_misses': [], 'hook_budget_misses': 0, 'bad_outputs': 0}, 'b57': {'reps_with_misses': [], 'hook_budget_misses': 0, 'bad_outputs': 0}}
- порог шума A/A (p95 |Δ|/база): None
- проверки: {'r_full_standard_le_1_15': True, 'r_norules_standard_vs_b57_le_1_00': True, 'required_1_0': False, 'd_hint_recall_lo_ge_-0_02': True, 'd_hint_recall_strict_lo_ge_-0_02': True, 'idea_hint_recall_1_0': True, 'idea_hints_match_b57': True, 'no_reps_with_misses': True, 'same_utc_day': True}

## ТС-1 к B4, CUDA fp16, по среднему на сценарий

- arm: 2026-11-15-m6-bge-m3-fp16-tokens-t13b-m6b-r1-srv0.0.10-m5.7+m6-holdout×3
- base: 2026-11-15-m6-bge-m3-fp16-tokens-t13b-b4-r1-srv0.0.5-m4-holdout×3
- scenarios: 15
- excluded: ['s-19', 's-44', 's-45']
- r: {'mean': 0.8876865918793769, 'lo': 0.8330168223380012, 'hi': 0.9194393088647699}
- base_micro: 0.8481012658227848
- arm_micro: 0.810126582278481
- d_micro: {'mean': -0.037974683544303806, 'lo': -0.08235294117647061, 'hi': 0.0}
- net_losses: 0
- net_losses_all_reps: 1
- noise_base_p95: 0.0060432196166637
- noise_arm_p95: 0.0542337134992712
- checks: {'d_micro_lo_ge_-0_02': False, 'net_losses_le_1': True, 'r_hi_lt_1_00': True, 'goal_r_le_0_70': False}

## ТС-1 к B4, int8 CPU (контроль)

- arm: 2026-11-14-m6-bge-m3-int8-tokens-t13b-cpu-m6b-srv0.0.10-m5.7+m6-holdout
- base: 2026-11-14-m6-bge-m3-int8-tokens-t13b-cpu-b4-srv0.0.5-m4-holdout
- split: holdout
- scenarios: 15
- base_tokens: 55394
- arm_tokens: 49271
- r: {'mean': 0.8894645629490558, 'lo': 0.8337614698989837, 'hi': 0.9220884780124095}
- base_micro: 0.8481012658227848
- arm_micro: 0.8227848101265823
- d_micro: {'mean': -0.02531645569620253, 'lo': -0.07228915662650602, 'hi': 0.0}
- net_losses: 0
- noninferior: False
- l0_excluded: None
- checks: {'d_micro_lo_ge_-0_02': False, 'net_losses_le_1': True}

- контроль v0.0.9 ≡ v0.0.10-m5.7 (FTS, holdout): {'verdict': '18 из 18 сценариев побайтово после канонизации id (в том числе a:<32 hex>), .tmp/m6/task13b/tools/control_a32.py', 'files': ['.tmp/m6/task13b/ts1/fts/2026-11-14-m6-fts-tokens-t13b-fts-b56-srv0.0.9-m5.6-holdout.json', '.tmp/m6/task13b/ts1/fts/2026-11-14-m6-fts-tokens-t13b-fts-b57-srv0.0.10-m5.7-holdout.json']}

## Качество к v0.0.9

- cuda-m6-r1-hybrid+rerank (hybrid+rerank): nDCG@10 {'mean': 0.871552494348287, 'lo': 0.8395707594002837, 'hi': 0.8998921931972262}, recall@5 {'mean': 0.9050613496932514, 'lo': 0.8736157975460123, 'hi': 0.9335391104294479}, reliable_first {'mean': 0.9285714285714286, 'lo': 0.7857142857142857, 'hi': 1.0}, сравнение {'against': '.tmp/m6/task13b/quality/cuda/b56-r1/2026-11-15-bge-m3-fp16-hybrid-rerank-t13b-q.json', 'n': 326, 'd_ndcg10': {'mean': -0.0029734796928634366, 'lo': -0.009202453987730062, 'hi': 0.0002820149091397498}, 'd_recall5': {'mean': 0.0, 'lo': -0.009202453987730062, 'hi': 0.009202453987730062}, 'regression': False}, проверки {'d_ndcg10_lo_ge_-0_01': True, 'd_recall5_lo_ge_-0_01': True}
- cuda-m6-r1-hybrid (hybrid): nDCG@10 {'mean': 0.8184013678003794, 'lo': 0.7818239282227748, 'hi': 0.8543007009335039}, recall@5 {'mean': 0.8449897750511248, 'lo': 0.8060250511247443, 'hi': 0.8833333333333335}, reliable_first {'mean': 1.0, 'lo': 1.0, 'hi': 1.0}, сравнение {'against': '.tmp/m6/task13b/quality/cuda/b56-r1/2026-11-15-bge-m3-fp16-hybrid-t13b-q.json', 'n': 326, 'd_ndcg10': {'mean': 0.0018063332364356124, 'lo': -0.0008992171227806611, 'hi': 0.005955488525923522}, 'd_recall5': {'mean': 0.0, 'lo': 0.0, 'hi': 0.0}, 'regression': False}, проверки {'d_ndcg10_lo_ge_-0_01': True, 'd_recall5_lo_ge_-0_01': True}
- cuda-m6-r2-hybrid+rerank (hybrid+rerank): nDCG@10 {'mean': 0.871552494348287, 'lo': 0.8395707594002837, 'hi': 0.8998921931972262}, recall@5 {'mean': 0.9050613496932514, 'lo': 0.8736157975460123, 'hi': 0.9335391104294479}, reliable_first {'mean': 0.9285714285714286, 'lo': 0.7857142857142857, 'hi': 1.0}, сравнение {'against': '.tmp/m6/task13b/quality/cuda/b56-r2/2026-11-15-bge-m3-fp16-hybrid-rerank-t13b-q.json', 'n': 326, 'd_ndcg10': {'mean': -0.0005775706323680089, 'lo': -0.00184282081746685, 'hi': 0.00011010892036282237}, 'd_recall5': {'mean': 0.0, 'lo': 0.0, 'hi': 0.0}, 'regression': False}, проверки {'d_ndcg10_lo_ge_-0_01': True, 'd_recall5_lo_ge_-0_01': True}
- cuda-m6-r2-hybrid (hybrid): nDCG@10 {'mean': 0.8184013678003794, 'lo': 0.7818239282227748, 'hi': 0.8543007009335039}, recall@5 {'mean': 0.8449897750511248, 'lo': 0.8060250511247443, 'hi': 0.8833333333333335}, reliable_first {'mean': 1.0, 'lo': 1.0, 'hi': 1.0}, сравнение {'against': '.tmp/m6/task13b/quality/cuda/b56-r2/2026-11-15-bge-m3-fp16-hybrid-t13b-q.json', 'n': 326, 'd_ndcg10': {'mean': 0.001307795915116302, 'lo': -0.0026999413548051345, 'hi': 0.005735028016463244}, 'd_recall5': {'mean': 0.0, 'lo': 0.0, 'hi': 0.0}, 'regression': False}, проверки {'d_ndcg10_lo_ge_-0_01': True, 'd_recall5_lo_ge_-0_01': True}
- cuda-m6-r3-hybrid+rerank (hybrid+rerank): nDCG@10 {'mean': 0.871552494348287, 'lo': 0.8395707594002837, 'hi': 0.8998921931972262}, recall@5 {'mean': 0.9050613496932514, 'lo': 0.8736157975460123, 'hi': 0.9335391104294479}, reliable_first {'mean': 0.9285714285714286, 'lo': 0.7857142857142857, 'hi': 1.0}, сравнение {'against': '.tmp/m6/task13b/quality/cuda/b56-r3/2026-11-15-bge-m3-fp16-hybrid-rerank-t13b-q.json', 'n': 326, 'd_ndcg10': {'mean': -0.0030674846625766868, 'lo': -0.009202453987730062, 'hi': 1.7027960500385836e-18}, 'd_recall5': {'mean': -0.003067484662576687, 'lo': -0.009202453987730062, 'hi': 0.0}, 'regression': False}, проверки {'d_ndcg10_lo_ge_-0_01': True, 'd_recall5_lo_ge_-0_01': True}
- cuda-m6-r3-hybrid (hybrid): nDCG@10 {'mean': 0.8184013678003794, 'lo': 0.7818239282227748, 'hi': 0.8543007009335039}, recall@5 {'mean': 0.8449897750511248, 'lo': 0.8060250511247443, 'hi': 0.8833333333333335}, reliable_first {'mean': 1.0, 'lo': 1.0, 'hi': 1.0}, сравнение {'against': '.tmp/m6/task13b/quality/cuda/b56-r3/2026-11-15-bge-m3-fp16-hybrid-t13b-q.json', 'n': 326, 'd_ndcg10': {'mean': 9.872734123680199e-05, 'lo': -0.0036750625378469967, 'hi': 0.004046878931114247}, 'd_recall5': {'mean': 0.0, 'lo': 0.0, 'hi': 0.0}, 'regression': False}, проверки {'d_ndcg10_lo_ge_-0_01': True, 'd_recall5_lo_ge_-0_01': True}
- cpu-m6-r1-hybrid (hybrid): nDCG@10 {'mean': 0.8165060111288376, 'lo': 0.7798427188616378, 'hi': 0.8523825303447841}, recall@5 {'mean': 0.8455010224948875, 'lo': 0.8071472392638036, 'hi': 0.883957055214724}, reliable_first {'mean': 1.0, 'lo': 1.0, 'hi': 1.0}, сравнение {'against': '.tmp/m6/task13b/quality/cpu/b56-r1/2026-11-14-bge-m3-int8-hybrid-t13b-qcpu.json', 'n': 326, 'd_ndcg10': {'mean': 0.001283264265862186, 'lo': -0.0017293144610687713, 'hi': 0.0050794836379096705}, 'd_recall5': {'mean': 0.0, 'lo': 0.0, 'hi': 0.0}, 'regression': False}, проверки {'d_ndcg10_lo_ge_-0_01': True, 'd_recall5_lo_ge_-0_01': True}

## Файлы

- `quality.cpu-m6-r1-hybrid` — `.tmp/m6/task13b/quality/cpu/m6-r1/2026-11-14-bge-m3-int8-hybrid-t13b-qcpu.json` (598350 байт, sha256 `fcd2d4b262bfe008…`)
- `quality.cuda-m6-r1-hybrid` — `.tmp/m6/task13b/quality/cuda/m6-r1/2026-11-15-bge-m3-fp16-hybrid-t13b-q.json` (599018 байт, sha256 `8e1e76c5bcb5b831…`)
- `quality.cuda-m6-r1-hybrid+rerank` — `.tmp/m6/task13b/quality/cuda/m6-r1/2026-11-15-bge-m3-fp16-hybrid-rerank-t13b-q.json` (597122 байт, sha256 `eedadfc63f7ee34e…`)
- `quality.cuda-m6-r2-hybrid` — `.tmp/m6/task13b/quality/cuda/m6-r2/2026-11-15-bge-m3-fp16-hybrid-t13b-q.json` (599150 байт, sha256 `7aa43703e1044da8…`)
- `quality.cuda-m6-r2-hybrid+rerank` — `.tmp/m6/task13b/quality/cuda/m6-r2/2026-11-15-bge-m3-fp16-hybrid-rerank-t13b-q.json` (597060 байт, sha256 `0e1b9046b200e3ff…`)
- `quality.cuda-m6-r3-hybrid` — `.tmp/m6/task13b/quality/cuda/m6-r3/2026-11-15-bge-m3-fp16-hybrid-t13b-q.json` (599071 байт, sha256 `4c6d6cfc7946bdc0…`)
- `quality.cuda-m6-r3-hybrid+rerank` — `.tmp/m6/task13b/quality/cuda/m6-r3/2026-11-15-bge-m3-fp16-hybrid-rerank-t13b-q.json` (597020 байт, sha256 `f1bfd96aa37019fa…`)
- `raw.quality.cpu.b56-r1.hybrid` — `.tmp/m6/task13b/quality/cpu/b56-r1/2026-11-14-bge-m3-int8-hybrid-t13b-qcpu.json` (597458 байт, sha256 `b79cf466648973ce…`)
- `raw.quality.cuda.b56-r1.hybrid` — `.tmp/m6/task13b/quality/cuda/b56-r1/2026-11-15-bge-m3-fp16-hybrid-t13b-q.json` (598254 байт, sha256 `818315205e5d7d95…`)
- `raw.quality.cuda.b56-r1.hybrid+rerank` — `.tmp/m6/task13b/quality/cuda/b56-r1/2026-11-15-bge-m3-fp16-hybrid-rerank-t13b-q.json` (596378 байт, sha256 `2502c03cd1d22503…`)
- `raw.quality.cuda.b56-r2.hybrid` — `.tmp/m6/task13b/quality/cuda/b56-r2/2026-11-15-bge-m3-fp16-hybrid-t13b-q.json` (598455 байт, sha256 `ff6e1de052099c74…`)
- `raw.quality.cuda.b56-r2.hybrid+rerank` — `.tmp/m6/task13b/quality/cuda/b56-r2/2026-11-15-bge-m3-fp16-hybrid-rerank-t13b-q.json` (596391 байт, sha256 `8ecb48c0770ff7c7…`)
- `raw.quality.cuda.b56-r3.hybrid` — `.tmp/m6/task13b/quality/cuda/b56-r3/2026-11-15-bge-m3-fp16-hybrid-t13b-q.json` (598492 байт, sha256 `1501d3ceb03ca55a…`)
- `raw.quality.cuda.b56-r3.hybrid+rerank` — `.tmp/m6/task13b/quality/cuda/b56-r3/2026-11-15-bge-m3-fp16-hybrid-rerank-t13b-q.json` (596264 байт, sha256 `cad42cac99e1f03d…`)
- `raw.ts1.-fts-b56-srv0.0.9-m5.6-holdout.json` — `.tmp/m6/task13b/ts1/fts/2026-11-14-m6-fts-tokens-t13b-fts-b56-srv0.0.9-m5.6-holdout.json` (1129076 байт, sha256 `fe3e381b5d40db0b…`)
- `raw.ts1.-fts-b57-srv0.0.10-m5.7-holdout.json` — `.tmp/m6/task13b/ts1/fts/2026-11-14-m6-fts-tokens-t13b-fts-b57-srv0.0.10-m5.7-holdout.json` (1129090 байт, sha256 `fcae506c31657f45…`)
- `raw.ts1.ens-t13b-aa-dev-srv0.0.10-m5.7+m6-dev-default-` — `.tmp/m6/task13b/ts1/dev-aa/2026-11-15-m6-bge-m3-fp16-tokens-t13b-aa-dev-srv0.0.10-m5.7+m6-dev-default-aa.json` (2195914 байт, sha256 `9af5e5823ce791a3…`)
- `raw.ts1.ens-t13b-aa-dev-srv0.0.10-m5.7+m6-dev-default.` — `.tmp/m6/task13b/ts1/dev-aa/2026-11-15-m6-bge-m3-fp16-tokens-t13b-aa-dev-srv0.0.10-m5.7+m6-dev-default.json` (2195919 байт, sha256 `c06ff3f31d117301…`)
- `raw.ts1.ens-t13b-b4-r1-srv0.0.5-m4-holdout.json` — `.tmp/m6/task13b/ts1/cuda/2026-11-15-m6-bge-m3-fp16-tokens-t13b-b4-r1-srv0.0.5-m4-holdout.json` (1154219 байт, sha256 `28187e7a0ea812eb…`)
- `raw.ts1.ens-t13b-b4-r2-srv0.0.5-m4-holdout.json` — `.tmp/m6/task13b/ts1/cuda/2026-11-15-m6-bge-m3-fp16-tokens-t13b-b4-r2-srv0.0.5-m4-holdout.json` (1152015 байт, sha256 `b1d29d7ac5191773…`)
- `raw.ts1.ens-t13b-b4-r3-srv0.0.5-m4-holdout.json` — `.tmp/m6/task13b/ts1/cuda/2026-11-15-m6-bge-m3-fp16-tokens-t13b-b4-r3-srv0.0.5-m4-holdout.json` (1152221 байт, sha256 `f57ee85162f26c25…`)
- `raw.ts1.ens-t13b-cpu-b4-srv0.0.5-m4-holdout.json` — `.tmp/m6/task13b/ts1/cpu/2026-11-14-m6-bge-m3-int8-tokens-t13b-cpu-b4-srv0.0.5-m4-holdout.json` (1149771 байт, sha256 `61ac09ceed5efec1…`)
- `raw.ts1.ens-t13b-cpu-m6-srv0.0.10-m5.7+m6-holdout.json` — `.tmp/m6/task13b/ts1/cpu/2026-11-14-m6-bge-m3-int8-tokens-t13b-cpu-m6-srv0.0.10-m5.7+m6-holdout.json` (1353422 байт, sha256 `70217e45cc93cf74…`)
- `raw.ts1.ens-t13b-cpu-m6b-srv0.0.10-m5.7+m6-holdout.jso` — `.tmp/m6/task13b/ts1/cpu/2026-11-14-m6-bge-m3-int8-tokens-t13b-cpu-m6b-srv0.0.10-m5.7+m6-holdout.json` (1315495 байт, sha256 `84434615511e5ec0…`)
- `raw.ts1.ens-t13b-m6-r1-srv0.0.10-m5.7+m6-holdout.json` — `.tmp/m6/task13b/ts1/cuda/2026-11-15-m6-bge-m3-fp16-tokens-t13b-m6-r1-srv0.0.10-m5.7+m6-holdout.json` (1350217 байт, sha256 `8689f6f2c401708b…`)
- `raw.ts1.ens-t13b-m6-r2-srv0.0.10-m5.7+m6-holdout.json` — `.tmp/m6/task13b/ts1/cuda/2026-11-15-m6-bge-m3-fp16-tokens-t13b-m6-r2-srv0.0.10-m5.7+m6-holdout.json` (1335607 байт, sha256 `524ac9be5513a6dd…`)
- `raw.ts1.ens-t13b-m6-r3-srv0.0.10-m5.7+m6-holdout.json` — `.tmp/m6/task13b/ts1/cuda/2026-11-15-m6-bge-m3-fp16-tokens-t13b-m6-r3-srv0.0.10-m5.7+m6-holdout.json` (1349132 байт, sha256 `467f007070766390…`)
- `raw.ts1.ens-t13b-m6b-r1-srv0.0.10-m5.7+m6-holdout.json` — `.tmp/m6/task13b/ts1/cuda/2026-11-15-m6-bge-m3-fp16-tokens-t13b-m6b-r1-srv0.0.10-m5.7+m6-holdout.json` (1312347 байт, sha256 `6193ee745ca56a5d…`)
- `raw.ts1.ens-t13b-m6b-r2-srv0.0.10-m5.7+m6-holdout.json` — `.tmp/m6/task13b/ts1/cuda/2026-11-15-m6-bge-m3-fp16-tokens-t13b-m6b-r2-srv0.0.10-m5.7+m6-holdout.json` (1324052 байт, sha256 `96be3181ded681a9…`)
- `raw.ts1.ens-t13b-m6b-r3-srv0.0.10-m5.7+m6-holdout.json` — `.tmp/m6/task13b/ts1/cuda/2026-11-15-m6-bge-m3-fp16-tokens-t13b-m6b-r3-srv0.0.10-m5.7+m6-holdout.json` (1318482 байт, sha256 `268b84f3eb6810d4…`)
- `raw.ts2.claude.a1.b56-claude-a1-srv0.0.9-m5.6-ho` — `.tmp/m6/task13b/ts2/claude-a1/2026-11-15-m6-bge-m3-fp16-sessions-t13b-b56-claude-a1-srv0.0.9-m5.6-holdout.json` (365028 байт, sha256 `4cd7fb28bc90536c…`)
- `raw.ts2.claude.a1.b57-claude-a1-srv0.0.10-m5.7-h` — `.tmp/m6/task13b/ts2/claude-a1/2026-11-15-m6-bge-m3-fp16-sessions-t13b-b57-claude-a1-srv0.0.10-m5.7-holdout.json` (380581 байт, sha256 `73198d8befb26271…`)
- `raw.ts2.claude.a1.m6-claude-a1-srv0.0.10-m5.7+m6` — `.tmp/m6/task13b/ts2/claude-a1/2026-11-15-m6-bge-m3-fp16-sessions-t13b-m6-claude-a1-srv0.0.10-m5.7+m6-holdout.json` (427021 байт, sha256 `2adb3ed000d8b61e…`)
- `raw.ts2.claude.a1.m6aa-claude-a1-srv0.0.10-m5.7+` — `.tmp/m6/task13b/ts2/claude-a1/2026-11-15-m6-bge-m3-fp16-sessions-t13b-m6aa-claude-a1-srv0.0.10-m5.7+m6-holdout.json` (428955 байт, sha256 `a75ce4a8746a9740…`)
- `raw.ts2.claude.a2.b56-claude-a2-srv0.0.9-m5.6-ho` — `.tmp/m6/task13b/ts2/claude-a2/2026-11-15-m6-bge-m3-fp16-sessions-t13b-b56-claude-a2-srv0.0.9-m5.6-holdout.json` (365556 байт, sha256 `a82318afe865a3ad…`)
- `raw.ts2.claude.a2.b57-claude-a2-srv0.0.10-m5.7-h` — `.tmp/m6/task13b/ts2/claude-a2/2026-11-15-m6-bge-m3-fp16-sessions-t13b-b57-claude-a2-srv0.0.10-m5.7-holdout.json` (380455 байт, sha256 `97000cc9b7d6b6eb…`)
- `raw.ts2.claude.a2.m6-claude-a2-srv0.0.10-m5.7+m6` — `.tmp/m6/task13b/ts2/claude-a2/2026-11-15-m6-bge-m3-fp16-sessions-t13b-m6-claude-a2-srv0.0.10-m5.7+m6-holdout.json` (428066 байт, sha256 `f58cea165c7501b3…`)
- `raw.ts2.claude.a2.m6aa-claude-a2-srv0.0.10-m5.7+` — `.tmp/m6/task13b/ts2/claude-a2/2026-11-15-m6-bge-m3-fp16-sessions-t13b-m6aa-claude-a2-srv0.0.10-m5.7+m6-holdout.json` (428602 байт, sha256 `35b2812f33a04756…`)
- `raw.ts2.codex.a1.b56-codex-a1-srv0.0.9-m5.6-hol` — `.tmp/m6/task13b/ts2/codex-a1/2026-11-15-m6-bge-m3-fp16-sessions-t13b-b56-codex-a1-srv0.0.9-m5.6-holdout.json` (365425 байт, sha256 `8ad69f88c3d26291…`)
- `raw.ts2.codex.a1.b57-codex-a1-srv0.0.10-m5.7-ho` — `.tmp/m6/task13b/ts2/codex-a1/2026-11-15-m6-bge-m3-fp16-sessions-t13b-b57-codex-a1-srv0.0.10-m5.7-holdout.json` (380385 байт, sha256 `22655779c4f959d1…`)
- `raw.ts2.codex.a1.m6-codex-a1-srv0.0.10-m5.7+m6-` — `.tmp/m6/task13b/ts2/codex-a1/2026-11-15-m6-bge-m3-fp16-sessions-t13b-m6-codex-a1-srv0.0.10-m5.7+m6-holdout.json` (404730 байт, sha256 `de1fd31ba0f0a47a…`)
- `ts1.control.0` — `.tmp/m6/task13b/ts1/fts/2026-11-14-m6-fts-tokens-t13b-fts-b56-srv0.0.9-m5.6-holdout.json` (1129076 байт, sha256 `fe3e381b5d40db0b…`)
- `ts1.control.1` — `.tmp/m6/task13b/ts1/fts/2026-11-14-m6-fts-tokens-t13b-fts-b57-srv0.0.10-m5.7-holdout.json` (1129090 байт, sha256 `fcae506c31657f45…`)
- `ts1.cpu` — `eval/results/2026-11-14-m6-bge-m3-int8-tokens-t13b-cpu-m6b-srv0.0.10-m5.7+m6-holdout-vs-2026-11-14-m6-bge-m3-int8-tokens-t13b-cpu-b4-srv0.0.5-m4-holdout.json` (947 байт, sha256 `e0d21a5393d4f0a2…`)
- `ts1.cpu.variant_a` — `eval/results/2026-11-14-m6-bge-m3-int8-tokens-t13b-cpu-m6-srv0.0.10-m5.7+m6-holdout-vs-2026-11-14-m6-bge-m3-int8-tokens-t13b-cpu-b4-srv0.0.5-m4-holdout.json` (945 байт, sha256 `22c68190a532fd63…`)
- `ts1.cuda` — `eval/results/2026-11-15-m6-bge-m3-fp16-tokens-t13b-m6b-r1-srv0.0.10-m5.7+m6-holdout-mean3-vs-2026-11-15-m6-bge-m3-fp16-tokens-t13b-b4-r1-srv0.0.5-m4-holdout-mean3.json` (3130 байт, sha256 `8a2ae0792870e917…`)
- `ts1.cuda.variant_a` — `eval/results/2026-11-15-m6-bge-m3-fp16-tokens-t13b-m6-r1-srv0.0.10-m5.7+m6-holdout-mean3-vs-2026-11-15-m6-bge-m3-fp16-tokens-t13b-b4-r1-srv0.0.5-m4-holdout-mean3.json` (3121 байт, sha256 `e4984940c9cc62b3…`)
- `ts1.dev_aa` — `eval/results/2026-11-15-m6-bge-m3-fp16-tokens-t13b-aa-dev-srv0.0.10-m5.7+m6-dev-default-aa-vs-default.json` (789 байт, sha256 `498d1e53ba819790…`)
- `ts2.claude.a1.vs_b56` — `eval/results/2026-11-15-m6-bge-m3-fp16-sessions-t13b-m6-claude-a1-srv0.0.10-m5.7+m6-holdout-vs-2026-11-15-m6-bge-m3-fp16-sessions-t13b-b56-claude-a1-srv0.0.9-m5.6-holdout.json` (52574 байт, sha256 `64902c308cfc1cd2…`)
- `ts2.claude.a1.vs_b57` — `eval/results/2026-11-15-m6-bge-m3-fp16-sessions-t13b-m6-claude-a1-srv0.0.10-m5.7+m6-holdout-vs-2026-11-15-m6-bge-m3-fp16-sessions-t13b-b57-claude-a1-srv0.0.10-m5.7-holdout.json` (56633 байт, sha256 `e20d0ad80b6942ab…`)
- `ts2.claude.aa` — `.tmp/m6/task13b/ts2/claude-a2/2026-11-15-m6-bge-m3-fp16-sessions-t13b-m6aa-claude-a2-srv0.0.10-m5.7+m6-holdout.json` (428602 байт, sha256 `35b2812f33a04756…`)
- `ts2.claude.aa_vs_m6` — `eval/results/2026-11-15-m6-bge-m3-fp16-sessions-t13b-m6aa-claude-a2-srv0.0.10-m5.7+m6-holdout-vs-2026-11-15-m6-bge-m3-fp16-sessions-t13b-m6-claude-a2-srv0.0.10-m5.7+m6-holdout.json` (50224 байт, sha256 `73794e4eebac842f…`)
- `ts2.claude.b56` — `.tmp/m6/task13b/ts2/claude-a2/2026-11-15-m6-bge-m3-fp16-sessions-t13b-b56-claude-a2-srv0.0.9-m5.6-holdout.json` (365556 байт, sha256 `a82318afe865a3ad…`)
- `ts2.claude.b57` — `.tmp/m6/task13b/ts2/claude-a2/2026-11-15-m6-bge-m3-fp16-sessions-t13b-b57-claude-a2-srv0.0.10-m5.7-holdout.json` (380455 байт, sha256 `97000cc9b7d6b6eb…`)
- `ts2.claude.b57_vs_b56` — `eval/results/2026-11-15-m6-bge-m3-fp16-sessions-t13b-b57-claude-a2-srv0.0.10-m5.7-holdout-vs-2026-11-15-m6-bge-m3-fp16-sessions-t13b-b56-claude-a2-srv0.0.9-m5.6-holdout.json` (58189 байт, sha256 `505ddf81af03bcc1…`)
- `ts2.claude.m6` — `.tmp/m6/task13b/ts2/claude-a2/2026-11-15-m6-bge-m3-fp16-sessions-t13b-m6-claude-a2-srv0.0.10-m5.7+m6-holdout.json` (428066 байт, sha256 `f58cea165c7501b3…`)
- `ts2.claude.vs_b56` — `eval/results/2026-11-15-m6-bge-m3-fp16-sessions-t13b-m6-claude-a2-srv0.0.10-m5.7+m6-holdout-vs-2026-11-15-m6-bge-m3-fp16-sessions-t13b-b56-claude-a2-srv0.0.9-m5.6-holdout.json` (51692 байт, sha256 `0d1b0b4785e82d2e…`)
- `ts2.claude.vs_b57` — `eval/results/2026-11-15-m6-bge-m3-fp16-sessions-t13b-m6-claude-a2-srv0.0.10-m5.7+m6-holdout-vs-2026-11-15-m6-bge-m3-fp16-sessions-t13b-b57-claude-a2-srv0.0.10-m5.7-holdout.json` (56259 байт, sha256 `8ad8a773b1fa6513…`)
- `ts2.codex.b56` — `.tmp/m6/task13b/ts2/codex-a1/2026-11-15-m6-bge-m3-fp16-sessions-t13b-b56-codex-a1-srv0.0.9-m5.6-holdout.json` (365425 байт, sha256 `8ad69f88c3d26291…`)
- `ts2.codex.b57` — `.tmp/m6/task13b/ts2/codex-a1/2026-11-15-m6-bge-m3-fp16-sessions-t13b-b57-codex-a1-srv0.0.10-m5.7-holdout.json` (380385 байт, sha256 `22655779c4f959d1…`)
- `ts2.codex.b57_vs_b56` — `eval/results/2026-11-15-m6-bge-m3-fp16-sessions-t13b-b57-codex-a1-srv0.0.10-m5.7-holdout-vs-2026-11-15-m6-bge-m3-fp16-sessions-t13b-b56-codex-a1-srv0.0.9-m5.6-holdout.json` (58397 байт, sha256 `8ea66caa5b6b1ffd…`)
- `ts2.codex.m6` — `.tmp/m6/task13b/ts2/codex-a1/2026-11-15-m6-bge-m3-fp16-sessions-t13b-m6-codex-a1-srv0.0.10-m5.7+m6-holdout.json` (404730 байт, sha256 `de1fd31ba0f0a47a…`)
- `ts2.codex.vs_b56` — `eval/results/2026-11-15-m6-bge-m3-fp16-sessions-t13b-m6-codex-a1-srv0.0.10-m5.7+m6-holdout-vs-2026-11-15-m6-bge-m3-fp16-sessions-t13b-b56-codex-a1-srv0.0.9-m5.6-holdout.json` (51907 байт, sha256 `ed637c54371da206…`)
- `ts2.codex.vs_b57` — `eval/results/2026-11-15-m6-bge-m3-fp16-sessions-t13b-m6-codex-a1-srv0.0.10-m5.7+m6-holdout-vs-2026-11-15-m6-bge-m3-fp16-sessions-t13b-b57-codex-a1-srv0.0.10-m5.7-holdout.json` (56269 байт, sha256 `935ba3d6a626e019…`)
