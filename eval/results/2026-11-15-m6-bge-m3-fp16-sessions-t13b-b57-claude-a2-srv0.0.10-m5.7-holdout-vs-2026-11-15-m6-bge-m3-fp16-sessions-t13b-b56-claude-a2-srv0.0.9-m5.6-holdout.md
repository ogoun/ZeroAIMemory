
## ТС-2: 2026-11-15-m6-bge-m3-fp16-sessions-t13b-b57-claude-a2-srv0.0.10-m5.7-holdout (0.0.10-m5.7) против 2026-11-15-m6-bge-m3-fp16-sessions-t13b-b56-claude-a2-srv0.0.9-m5.6-holdout (0.0.9-m5.6)

- исключено из обоих плеч: события {"session_start_after_abort"}; классы строк {}; обязательные строки {"idea"}; ожидания {"idea"}

### всего (сессий 8)

| Событие | база | плечо | R |
|---|---:|---:|---|
| checkpoint | 0 | 0 | — |
| file_context | 0 | 0 | — |
| post_tool | 0 | 0 | — |
| pre_tool | 547 | 547 | 1.000 [1.000–1.000] |
| prompt | 1494 | 1628 | 1.090 [1.000–1.115] |
| question | 5996 | 5998 | 1.000 [1.000–1.001] |
| repeat | 1438 | 1439 | 1.001 [1.001–1.001] |
| session_start | 4305 | 4854 | 1.128 [1.044–1.301] |
| subagent_mcp | 1242 | 1242 | 1.000 [1.000–1.000] |
| subagent_start | 825 | 825 | 1.000 [1.000–1.000] |
| всего | 15846 | 16534 | 1.043 [1.024–1.081] |

- Δ (плечо − база, общие элементы, доли сессий — среднее по повторам, односторонние 95 %): hint_recall 0.000 [0.000–0.000], строго 0.000 [0.000–0.000], обязательные строки -0.052 [-0.086–0.000]
- **база**: сессий 8 × 3 повторов, токенов 47539 (checkpoint 0, file_context 0, post_tool 0, pre_tool 1641, prompt 4482, question 17988, repeat 4313, session_start 12915, subagent_mcp 3725, subagent_start 2475); обязательные строки 186/213 (0.873); нагеты вопросов 33/33 (1.000); дайджест при двух стартах 24/24 (1.000)
  - hint_recall общий 87/87 (1.000) (строго в событии 36/87 (0.414)); подсказки-идеи — (строго —); по видам: insight 30/30 (1.000) (строго 30/30 (1.000)); rule 48/48 (1.000) (строго 0/48 (0.000)); term 9/9 (1.000) (строго 6/9 (0.667)); зачёт: digest 48, event 36, hooks 3
  - повторная выдача 102/522 (0.195) (между каналами 240/495 (0.485)); идеи: 0; снимок сессии: 0; раздел Rules: session_start 6969; шум повторов (p95 |Δ|/среднее сессии): 0.0007
  - обязательные строки, худший повтор: r0 62/71 (0.873)
  - нет строк: sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:oncall
- **плечо**: сессий 8 × 3 повторов, токенов 49602 (checkpoint 0, file_context 0, post_tool 0, pre_tool 1641, prompt 4884, question 17995, repeat 4318, session_start 14563, subagent_mcp 3726, subagent_start 2475); обязательные строки 175/213 (0.822); нагеты вопросов 33/33 (1.000); дайджест при двух стартах 24/24 (1.000)
  - hint_recall общий 87/87 (1.000) (строго в событии 36/87 (0.414)); подсказки-идеи — (строго —); по видам: insight 30/30 (1.000) (строго 30/30 (1.000)); rule 48/48 (1.000) (строго 0/48 (0.000)); term 9/9 (1.000) (строго 6/9 (0.667)); зачёт: digest 48, event 36, hooks 3
  - повторная выдача 102/559 (0.182) (между каналами 234/532 (0.440)); идеи: prompt 552, session_start 2217; снимок сессии: 0; раздел Rules: session_start 6287; шум повторов (p95 |Δ|/среднее сессии): 0.0036
  - обязательные строки, худший повтор: r0 58/71 (0.817)
  - нет строк: sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:frontend-a11y; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:i18n; sh-02 session_start rule:cache-headers; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:i18n; sh-02 session_start rule:cache-headers; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:frontend-a11y; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:i18n; sh-02 session_start rule:cache-headers; sh-02 session_start rule:oncall

### standard (сессий 6)

| Событие | база | плечо | R |
|---|---:|---:|---|
| checkpoint | 0 | 0 | — |
| file_context | 0 | 0 | — |
| post_tool | 0 | 0 | — |
| pre_tool | 368 | 368 | 1.000 [1.000–1.000] |
| prompt | 367 | 367 | 1.000 [1.000–1.000] |
| question | 3014 | 3015 | 1.000 [1.000–1.001] |
| repeat | 485 | 486 | 1.001 [1.001–1.001] |
| session_start | 1558 | 1957 | 1.256 [1.120–1.388] |
| subagent_mcp | 268 | 268 | 1.000 [1.000–1.000] |
| subagent_start | 167 | 167 | 1.000 [1.000–1.000] |
| всего | 6228 | 6628 | 1.064 [1.026–1.111] |

- Δ (плечо − база, общие элементы, доли сессий — среднее по повторам, односторонние 95 %): hint_recall 0.000 [0.000–0.000], строго 0.000 [0.000–0.000], обязательные строки 0.000 [0.000–0.000]
- **база**: сессий 6 × 3 повторов, токенов 18683 (checkpoint 0, file_context 0, post_tool 0, pre_tool 1104, prompt 1101, question 9042, repeat 1456, session_start 4674, subagent_mcp 805, subagent_start 501); обязательные строки 69/69 (1.000); нагеты вопросов 21/21 (1.000); дайджест при двух стартах 18/18 (1.000)
  - hint_recall общий 60/60 (1.000) (строго в событии 18/60 (0.300)); подсказки-идеи — (строго —); по видам: insight 18/18 (1.000) (строго 18/18 (1.000)); rule 42/42 (1.000) (строго 0/42 (0.000)); зачёт: digest 42, event 18
  - повторная выдача 36/201 (0.179) (между каналами 105/192 (0.547)); идеи: 0; снимок сессии: 0; раздел Rules: session_start 3027; шум повторов (p95 |Δ|/среднее сессии): 0.0008
  - обязательные строки, худший повтор: r0 23/23 (1.000)
- **плечо**: сессий 6 × 3 повторов, токенов 19885 (checkpoint 0, file_context 0, post_tool 0, pre_tool 1104, prompt 1101, question 9046, repeat 1457, session_start 5871, subagent_mcp 805, subagent_start 501); обязательные строки 69/69 (1.000); нагеты вопросов 21/21 (1.000); дайджест при двух стартах 18/18 (1.000)
  - hint_recall общий 60/60 (1.000) (строго в событии 18/60 (0.300)); подсказки-идеи — (строго —); по видам: insight 18/18 (1.000) (строго 18/18 (1.000)); rule 42/42 (1.000) (строго 0/42 (0.000)); зачёт: digest 42, event 18
  - повторная выдача 36/219 (0.164) (между каналами 105/210 (0.500)); идеи: session_start 1209; снимок сессии: 0; раздел Rules: session_start 3027; шум повторов (p95 |Δ|/среднее сессии): 0.0006
  - обязательные строки, худший повтор: r0 23/23 (1.000)

### heavy (сессий 2)

| Событие | база | плечо | R |
|---|---:|---:|---|
| checkpoint | 0 | 0 | — |
| file_context | 0 | 0 | — |
| post_tool | 0 | 0 | — |
| pre_tool | 179 | 179 | 1.000 [1.000–1.000] |
| prompt | 1127 | 1261 | 1.119 [1.110–1.134] |
| question | 2982 | 2983 | 1.000 [1.000–1.001] |
| repeat | 952 | 954 | 1.001 [1.001–1.001] |
| session_start | 2747 | 2897 | 1.055 [0.992–1.128] |
| subagent_mcp | 973 | 974 | 1.000 [1.000–1.000] |
| subagent_start | 658 | 658 | 1.000 [1.000–1.000] |
| всего | 9619 | 9906 | 1.030 [1.016–1.040] |

- Δ (плечо − база, общие элементы, доли сессий — среднее по повторам, односторонние 95 %): hint_recall 0.000 [0.000–0.000], строго 0.000 [0.000–0.000], обязательные строки -0.076 [-0.115–0.000]
- **база**: сессий 2 × 3 повторов, токенов 28856 (checkpoint 0, file_context 0, post_tool 0, pre_tool 537, prompt 3381, question 8946, repeat 2857, session_start 8241, subagent_mcp 2920, subagent_start 1974); обязательные строки 117/144 (0.812); нагеты вопросов 12/12 (1.000); дайджест при двух стартах 6/6 (1.000)
  - hint_recall общий 27/27 (1.000) (строго в событии 18/27 (0.667)); подсказки-идеи — (строго —); по видам: insight 12/12 (1.000) (строго 12/12 (1.000)); rule 6/6 (1.000) (строго 0/6 (0.000)); term 9/9 (1.000) (строго 6/9 (0.667)); зачёт: digest 6, event 18, hooks 3
  - повторная выдача 66/321 (0.206) (между каналами 135/303 (0.446)); идеи: 0; снимок сессии: 0; раздел Rules: session_start 3942; шум повторов (p95 |Δ|/среднее сессии): 0.0003
  - обязательные строки, худший повтор: r0 39/48 (0.812)
  - нет строк: sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:oncall
- **плечо**: сессий 2 × 3 повторов, токенов 29717 (checkpoint 0, file_context 0, post_tool 0, pre_tool 537, prompt 3783, question 8949, repeat 2861, session_start 8692, subagent_mcp 2921, subagent_start 1974); обязательные строки 106/144 (0.736); нагеты вопросов 12/12 (1.000); дайджест при двух стартах 6/6 (1.000)
  - hint_recall общий 27/27 (1.000) (строго в событии 18/27 (0.667)); подсказки-идеи — (строго —); по видам: insight 12/12 (1.000) (строго 12/12 (1.000)); rule 6/6 (1.000) (строго 0/6 (0.000)); term 9/9 (1.000) (строго 6/9 (0.667)); зачёт: digest 6, event 18, hooks 3
  - повторная выдача 66/340 (0.194) (между каналами 129/322 (0.401)); идеи: prompt 552, session_start 1008; снимок сессии: 0; раздел Rules: session_start 3260; шум повторов (p95 |Δ|/среднее сессии): 0.0061
  - обязательные строки, худший повтор: r0 35/48 (0.729)
  - нет строк: sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:frontend-a11y; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:i18n; sh-02 session_start rule:cache-headers; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:i18n; sh-02 session_start rule:cache-headers; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:frontend-a11y; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:i18n; sh-02 session_start rule:cache-headers; sh-02 session_start rule:oncall

### Без раздела Rules (резерв `session_start`/`recovery` вычтен в обоих плечах; §17 п. 2, решение 10)

| Часть | база | плечо | R | session_start R | recovery R |
|---|---:|---:|---|---|---|
| всего (8) | 13523 | 14438 | 1.068 [1.042–1.106] | 1.392 [1.270–1.885] | — |
| standard (6) | 5219 | 5619 | 1.077 [1.030–1.138] | 1.727 [1.315–2.630] | — |
| heavy (2) | 8305 | 8819 | 1.062 [1.045–1.085] | 1.264 [1.251–1.274] | — |

- |Δ| / база токенов сессии, 95-й перцентиль: 0.1285
- бутстреп: 10000 выборок, зерно 0x5eed20261019 (парный по сессиям; токены сессии — среднее по повторам).
- повторная выдача — справочно и смещена в пользу M6: повтор `/read` с пометкой «уже выдавалось» считается помеченным, у плеча без пометки (до M6) тот же повтор — неотмеченным.
