
## ТС-2: 2026-11-15-m6-bge-m3-fp16-sessions-t13b-m6-codex-a1-srv0.0.10-m5.7+m6-holdout (0.0.10-m5.7+m6) против 2026-11-15-m6-bge-m3-fp16-sessions-t13b-b56-codex-a1-srv0.0.9-m5.6-holdout (0.0.9-m5.6)

- исключено из обоих плеч: события {"session_start_after_abort"}; классы строк {}; обязательные строки {"idea"}; ожидания {"idea"}

### всего (сессий 8)

| Событие | база | плечо | R |
|---|---:|---:|---|
| checkpoint | 0 | 0 | — |
| file_context | 0 | 0 | — |
| post_tool | 0 | 0 | — |
| pre_tool | 547 | 538 | 0.984 [0.983–0.984] |
| prompt | 1494 | 1726 | 1.155 [0.989–1.234] |
| question | 5996 | 5546 | 0.925 [0.878–0.974] |
| repeat | 1437 | 1280 | 0.891 [0.864–0.944] |
| session_start | 4292 | 5958 | 1.388 [1.318–1.493] |
| subagent_mcp | 1242 | 998 | 0.804 [0.744–1.020] |
| subagent_start | 825 | 720 | 0.873 [0.856–0.877] |
| всего | 15834 | 16767 | 1.059 [0.987–1.141] |

- Δ (плечо − база, общие элементы, доли сессий — среднее по повторам, односторонние 95 %): hint_recall 0.000 [0.000–0.000], строго 0.000 [0.000–0.000], обязательные строки 0.131 [0.000–0.220]
- **база**: сессий 8 × 3 повторов, токенов 47501 (checkpoint 0, file_context 0, post_tool 0, pre_tool 1641, prompt 4482, question 17989, repeat 4311, session_start 12877, subagent_mcp 3726, subagent_start 2475); обязательные строки 185/213 (0.869); нагеты вопросов 33/33 (1.000); дайджест при двух стартах 24/24 (1.000)
  - hint_recall общий 87/87 (1.000) (строго в событии 36/87 (0.414)); подсказки-идеи — (строго —); по видам: insight 30/30 (1.000) (строго 30/30 (1.000)); rule 48/48 (1.000) (строго 0/48 (0.000)); term 9/9 (1.000) (строго 6/9 (0.667)); зачёт: digest 48, event 36, hooks 3
  - повторная выдача 102/521 (0.196) (между каналами 239/494 (0.484)); идеи: 0; снимок сессии: 0; раздел Rules: session_start 6915; шум повторов (p95 |Δ|/среднее сессии): 0.0031
  - обязательные строки, худший повтор: r1 61/71 (0.859)
  - нет строк: sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:oncall
- **плечо**: сессий 8 × 3 повторов, токенов 50300 (checkpoint 0, file_context 0, post_tool 0, pre_tool 1614, prompt 5178, question 16638, repeat 3841, session_start 17874, subagent_mcp 2995, subagent_start 2160); обязательные строки 213/213 (1.000); нагеты вопросов 33/33 (1.000); дайджест при двух стартах 24/24 (1.000)
  - hint_recall общий 87/87 (1.000) (строго в событии 36/87 (0.414)); подсказки-идеи — (строго —); по видам: insight 30/30 (1.000) (строго 30/30 (1.000)); rule 48/48 (1.000) (строго 0/48 (0.000)); term 9/9 (1.000) (строго 6/9 (0.667)); зачёт: digest 48, event 36, hooks 3
  - повторная выдача 93/551 (0.169) (между каналами 221/539 (0.410)); идеи: prompt 552, session_start 2217; снимок сессии: 0; раздел Rules: session_start 9216; шум повторов (p95 |Δ|/среднее сессии): 0.0108
  - обязательные строки, худший повтор: r0 71/71 (1.000)

### standard (сессий 6)

| Событие | база | плечо | R |
|---|---:|---:|---|
| checkpoint | 0 | 0 | — |
| file_context | 0 | 0 | — |
| post_tool | 0 | 0 | — |
| pre_tool | 368 | 362 | 0.984 [0.983–0.984] |
| prompt | 367 | 363 | 0.989 [0.987–0.990] |
| question | 3013 | 2923 | 0.970 [0.961–0.979] |
| repeat | 485 | 458 | 0.944 [0.944–0.944] |
| session_start | 1558 | 2249 | 1.444 [1.329–1.605] |
| subagent_mcp | 269 | 274 | 1.020 [1.020–1.020] |
| subagent_start | 167 | 143 | 0.856 [0.856–0.856] |
| всего | 6227 | 6772 | 1.088 [1.043–1.144] |

- Δ (плечо − база, общие элементы, доли сессий — среднее по повторам, односторонние 95 %): hint_recall 0.000 [0.000–0.000], строго 0.000 [0.000–0.000], обязательные строки 0.000 [0.000–0.000]
- **база**: сессий 6 × 3 повторов, токенов 18681 (checkpoint 0, file_context 0, post_tool 0, pre_tool 1104, prompt 1101, question 9040, repeat 1455, session_start 4674, subagent_mcp 806, subagent_start 501); обязательные строки 69/69 (1.000); нагеты вопросов 21/21 (1.000); дайджест при двух стартах 18/18 (1.000)
  - hint_recall общий 60/60 (1.000) (строго в событии 18/60 (0.300)); подсказки-идеи — (строго —); по видам: insight 18/18 (1.000) (строго 18/18 (1.000)); rule 42/42 (1.000) (строго 0/42 (0.000)); зачёт: digest 42, event 18
  - повторная выдача 36/201 (0.179) (между каналами 105/192 (0.547)); идеи: 0; снимок сессии: 0; раздел Rules: session_start 3027; шум повторов (p95 |Δ|/среднее сессии): 0.0007
  - обязательные строки, худший повтор: r0 23/23 (1.000)
- **плечо**: сессий 6 × 3 повторов, токенов 20317 (checkpoint 0, file_context 0, post_tool 0, pre_tool 1086, prompt 1089, question 8770, repeat 1374, session_start 6747, subagent_mcp 822, subagent_start 429); обязательные строки 69/69 (1.000); нагеты вопросов 21/21 (1.000); дайджест при двух стартах 18/18 (1.000)
  - hint_recall общий 60/60 (1.000) (строго в событии 18/60 (0.300)); подсказки-идеи — (строго —); по видам: insight 18/18 (1.000) (строго 18/18 (1.000)); rule 42/42 (1.000) (строго 0/42 (0.000)); зачёт: digest 42, event 18
  - повторная выдача 33/213 (0.155) (между каналами 102/207 (0.493)); идеи: session_start 1209; снимок сессии: 0; раздел Rules: session_start 3909; шум повторов (p95 |Δ|/среднее сессии): 0.0006
  - обязательные строки, худший повтор: r0 23/23 (1.000)

### heavy (сессий 2)

| Событие | база | плечо | R |
|---|---:|---:|---|
| checkpoint | 0 | 0 | — |
| file_context | 0 | 0 | — |
| post_tool | 0 | 0 | — |
| pre_tool | 179 | 176 | 0.983 [0.982–0.984] |
| prompt | 1127 | 1363 | 1.209 [1.113–1.268] |
| question | 2983 | 2623 | 0.879 [0.840–0.967] |
| repeat | 952 | 822 | 0.864 [0.864–0.864] |
| session_start | 2734 | 3709 | 1.356 [1.285–1.417] |
| subagent_mcp | 973 | 724 | 0.744 [0.744–0.744] |
| subagent_start | 658 | 577 | 0.877 [0.877–0.877] |
| всего | 9607 | 9994 | 1.040 [0.953–1.154] |

- Δ (плечо − база, общие элементы, доли сессий — среднее по повторам, односторонние 95 %): hint_recall 0.000 [0.000–0.000], строго 0.000 [0.000–0.000], обязательные строки 0.194 [0.000–0.292]
- **база**: сессий 2 × 3 повторов, токенов 28820 (checkpoint 0, file_context 0, post_tool 0, pre_tool 537, prompt 3381, question 8949, repeat 2856, session_start 8203, subagent_mcp 2920, subagent_start 1974); обязательные строки 116/144 (0.806); нагеты вопросов 12/12 (1.000); дайджест при двух стартах 6/6 (1.000)
  - hint_recall общий 27/27 (1.000) (строго в событии 18/27 (0.667)); подсказки-идеи — (строго —); по видам: insight 12/12 (1.000) (строго 12/12 (1.000)); rule 6/6 (1.000) (строго 0/6 (0.000)); term 9/9 (1.000) (строго 6/9 (0.667)); зачёт: digest 6, event 18, hooks 3
  - повторная выдача 66/320 (0.206) (между каналами 134/302 (0.444)); идеи: 0; снимок сессии: 0; раздел Rules: session_start 3888; шум повторов (p95 |Δ|/среднее сессии): 0.0053
  - обязательные строки, худший повтор: r1 38/48 (0.792)
  - нет строк: sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:oncall
- **плечо**: сессий 2 × 3 повторов, токенов 29983 (checkpoint 0, file_context 0, post_tool 0, pre_tool 528, prompt 4089, question 7868, repeat 2467, session_start 11127, subagent_mcp 2173, subagent_start 1731); обязательные строки 144/144 (1.000); нагеты вопросов 12/12 (1.000); дайджест при двух стартах 6/6 (1.000)
  - hint_recall общий 27/27 (1.000) (строго в событии 18/27 (0.667)); подсказки-идеи — (строго —); по видам: insight 12/12 (1.000) (строго 12/12 (1.000)); rule 6/6 (1.000) (строго 0/6 (0.000)); term 9/9 (1.000) (строго 6/9 (0.667)); зачёт: digest 6, event 18, hooks 3
  - повторная выдача 60/338 (0.178) (между каналами 119/332 (0.358)); идеи: prompt 552, session_start 1008; снимок сессии: 0; раздел Rules: session_start 5307; шум повторов (p95 |Δ|/среднее сессии): 0.0132
  - обязательные строки, худший повтор: r0 48/48 (1.000)

### Без раздела Rules (резерв `session_start`/`recovery` вычтен в обоих плечах; §17 п. 2, решение 10)

| Часть | база | плечо | R | session_start R | recovery R |
|---|---:|---:|---|---|---|
| всего (8) | 13529 | 13695 | 1.012 [0.941–1.100] | 1.452 [1.301–1.884] | — |
| standard (6) | 5218 | 5469 | 1.048 [0.994–1.112] | 1.723 [1.312–2.630] | — |
| heavy (2) | 8311 | 8225 | 0.990 [0.906–1.105] | 1.349 [1.249–1.430] | — |

- |Δ| / база токенов сессии, 95-й перцентиль: 0.1877
- бутстреп: 10000 выборок, зерно 0x5eed20261019 (парный по сессиям; токены сессии — среднее по повторам).
- повторная выдача — справочно и смещена в пользу M6: повтор `/read` с пометкой «уже выдавалось» считается помеченным, у плеча без пометки (до M6) тот же повтор — неотмеченным.
