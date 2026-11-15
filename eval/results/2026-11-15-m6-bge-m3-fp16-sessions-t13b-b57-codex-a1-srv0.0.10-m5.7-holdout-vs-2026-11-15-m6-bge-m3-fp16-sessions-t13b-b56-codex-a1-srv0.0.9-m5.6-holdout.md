
## ТС-2: 2026-11-15-m6-bge-m3-fp16-sessions-t13b-b57-codex-a1-srv0.0.10-m5.7-holdout (0.0.10-m5.7) против 2026-11-15-m6-bge-m3-fp16-sessions-t13b-b56-codex-a1-srv0.0.9-m5.6-holdout (0.0.9-m5.6)

- исключено из обоих плеч: события {"session_start_after_abort"}; классы строк {}; обязательные строки {"idea"}; ожидания {"idea"}

### всего (сессий 8)

| Событие | база | плечо | R |
|---|---:|---:|---|
| checkpoint | 0 | 0 | — |
| file_context | 0 | 0 | — |
| post_tool | 0 | 0 | — |
| pre_tool | 547 | 547 | 1.000 [1.000–1.000] |
| prompt | 1494 | 1643 | 1.100 [1.000–1.126] |
| question | 5996 | 5997 | 1.000 [1.000–1.001] |
| repeat | 1437 | 1438 | 1.001 [1.000–1.001] |
| session_start | 4292 | 4846 | 1.129 [1.046–1.301] |
| subagent_mcp | 1242 | 1242 | 1.000 [0.999–1.001] |
| subagent_start | 825 | 825 | 1.000 [1.000–1.000] |
| всего | 15834 | 16539 | 1.045 [1.026–1.080] |

- Δ (плечо − база, общие элементы, доли сессий — среднее по повторам, односторонние 95 %): hint_recall 0.000 [0.000–0.000], строго 0.000 [0.000–0.000], обязательные строки -0.047 [-0.078–0.000]
- **база**: сессий 8 × 3 повторов, токенов 47501 (checkpoint 0, file_context 0, post_tool 0, pre_tool 1641, prompt 4482, question 17989, repeat 4311, session_start 12877, subagent_mcp 3726, subagent_start 2475); обязательные строки 185/213 (0.869); нагеты вопросов 33/33 (1.000); дайджест при двух стартах 24/24 (1.000)
  - hint_recall общий 87/87 (1.000) (строго в событии 36/87 (0.414)); подсказки-идеи — (строго —); по видам: insight 30/30 (1.000) (строго 30/30 (1.000)); rule 48/48 (1.000) (строго 0/48 (0.000)); term 9/9 (1.000) (строго 6/9 (0.667)); зачёт: digest 48, event 36, hooks 3
  - повторная выдача 102/521 (0.196) (между каналами 239/494 (0.484)); идеи: 0; снимок сессии: 0; раздел Rules: session_start 6915; шум повторов (p95 |Δ|/среднее сессии): 0.0031
  - обязательные строки, худший повтор: r1 61/71 (0.859)
  - нет строк: sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:oncall
- **плечо**: сессий 8 × 3 повторов, токенов 49616 (checkpoint 0, file_context 0, post_tool 0, pre_tool 1641, prompt 4929, question 17990, repeat 4315, session_start 14539, subagent_mcp 3727, subagent_start 2475); обязательные строки 175/213 (0.822); нагеты вопросов 33/33 (1.000); дайджест при двух стартах 24/24 (1.000)
  - hint_recall общий 87/87 (1.000) (строго в событии 36/87 (0.414)); подсказки-идеи — (строго —); по видам: insight 30/30 (1.000) (строго 30/30 (1.000)); rule 48/48 (1.000) (строго 0/48 (0.000)); term 9/9 (1.000) (строго 6/9 (0.667)); зачёт: digest 48, event 36, hooks 3
  - повторная выдача 102/558 (0.183) (между каналами 234/531 (0.441)); идеи: prompt 552, session_start 2217; снимок сессии: 0; раздел Rules: session_start 6279; шум повторов (p95 |Δ|/среднее сессии): 0.0083
  - обязательные строки, худший повтор: r0 58/71 (0.817)
  - нет строк: sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:frontend-a11y; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:i18n; sh-02 session_start rule:cache-headers; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:frontend-a11y; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:i18n; sh-02 session_start rule:cache-headers; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:frontend-a11y; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:i18n; sh-02 session_start rule:cache-headers; sh-02 session_start rule:oncall

### standard (сессий 6)

| Событие | база | плечо | R |
|---|---:|---:|---|
| checkpoint | 0 | 0 | — |
| file_context | 0 | 0 | — |
| post_tool | 0 | 0 | — |
| pre_tool | 368 | 368 | 1.000 [1.000–1.000] |
| prompt | 367 | 367 | 1.000 [1.000–1.000] |
| question | 3013 | 3014 | 1.000 [1.000–1.001] |
| repeat | 485 | 485 | 1.000 [1.000–1.000] |
| session_start | 1558 | 1957 | 1.256 [1.120–1.388] |
| subagent_mcp | 269 | 268 | 0.999 [0.999–0.999] |
| subagent_start | 167 | 167 | 1.000 [1.000–1.000] |
| всего | 6227 | 6626 | 1.064 [1.025–1.110] |

- Δ (плечо − база, общие элементы, доли сессий — среднее по повторам, односторонние 95 %): hint_recall 0.000 [0.000–0.000], строго 0.000 [0.000–0.000], обязательные строки 0.000 [0.000–0.000]
- **база**: сессий 6 × 3 повторов, токенов 18681 (checkpoint 0, file_context 0, post_tool 0, pre_tool 1104, prompt 1101, question 9040, repeat 1455, session_start 4674, subagent_mcp 806, subagent_start 501); обязательные строки 69/69 (1.000); нагеты вопросов 21/21 (1.000); дайджест при двух стартах 18/18 (1.000)
  - hint_recall общий 60/60 (1.000) (строго в событии 18/60 (0.300)); подсказки-идеи — (строго —); по видам: insight 18/18 (1.000) (строго 18/18 (1.000)); rule 42/42 (1.000) (строго 0/42 (0.000)); зачёт: digest 42, event 18
  - повторная выдача 36/201 (0.179) (между каналами 105/192 (0.547)); идеи: 0; снимок сессии: 0; раздел Rules: session_start 3027; шум повторов (p95 |Δ|/среднее сессии): 0.0007
  - обязательные строки, худший повтор: r0 23/23 (1.000)
- **плечо**: сессий 6 × 3 повторов, токенов 19879 (checkpoint 0, file_context 0, post_tool 0, pre_tool 1104, prompt 1101, question 9042, repeat 1455, session_start 5871, subagent_mcp 805, subagent_start 501); обязательные строки 69/69 (1.000); нагеты вопросов 21/21 (1.000); дайджест при двух стартах 18/18 (1.000)
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
| prompt | 1127 | 1276 | 1.132 [1.131–1.134] |
| question | 2983 | 2983 | 1.000 [1.000–1.000] |
| repeat | 952 | 953 | 1.001 [1.001–1.001] |
| session_start | 2734 | 2889 | 1.057 [0.995–1.128] |
| subagent_mcp | 973 | 974 | 1.001 [1.001–1.001] |
| subagent_start | 658 | 658 | 1.000 [1.000–1.000] |
| всего | 9607 | 9912 | 1.032 [1.021–1.040] |

- Δ (плечо − база, общие элементы, доли сессий — среднее по повторам, односторонние 95 %): hint_recall 0.000 [0.000–0.000], строго 0.000 [0.000–0.000], обязательные строки -0.069 [-0.104–0.000]
- **база**: сессий 2 × 3 повторов, токенов 28820 (checkpoint 0, file_context 0, post_tool 0, pre_tool 537, prompt 3381, question 8949, repeat 2856, session_start 8203, subagent_mcp 2920, subagent_start 1974); обязательные строки 116/144 (0.806); нагеты вопросов 12/12 (1.000); дайджест при двух стартах 6/6 (1.000)
  - hint_recall общий 27/27 (1.000) (строго в событии 18/27 (0.667)); подсказки-идеи — (строго —); по видам: insight 12/12 (1.000) (строго 12/12 (1.000)); rule 6/6 (1.000) (строго 0/6 (0.000)); term 9/9 (1.000) (строго 6/9 (0.667)); зачёт: digest 6, event 18, hooks 3
  - повторная выдача 66/320 (0.206) (между каналами 134/302 (0.444)); идеи: 0; снимок сессии: 0; раздел Rules: session_start 3888; шум повторов (p95 |Δ|/среднее сессии): 0.0053
  - обязательные строки, худший повтор: r1 38/48 (0.792)
  - нет строк: sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:oncall
- **плечо**: сессий 2 × 3 повторов, токенов 29737 (checkpoint 0, file_context 0, post_tool 0, pre_tool 537, prompt 3828, question 8948, repeat 2860, session_start 8668, subagent_mcp 2922, subagent_start 1974); обязательные строки 106/144 (0.736); нагеты вопросов 12/12 (1.000); дайджест при двух стартах 6/6 (1.000)
  - hint_recall общий 27/27 (1.000) (строго в событии 18/27 (0.667)); подсказки-идеи — (строго —); по видам: insight 12/12 (1.000) (строго 12/12 (1.000)); rule 6/6 (1.000) (строго 0/6 (0.000)); term 9/9 (1.000) (строго 6/9 (0.667)); зачёт: digest 6, event 18, hooks 3
  - повторная выдача 66/339 (0.195) (между каналами 129/321 (0.402)); идеи: prompt 552, session_start 1008; снимок сессии: 0; раздел Rules: session_start 3252; шум повторов (p95 |Δ|/среднее сессии): 0.0125
  - обязательные строки, худший повтор: r0 35/48 (0.729)
  - нет строк: sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:frontend-a11y; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:i18n; sh-02 session_start rule:cache-headers; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:frontend-a11y; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:i18n; sh-02 session_start rule:cache-headers; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:frontend-a11y; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:i18n; sh-02 session_start rule:cache-headers; sh-02 session_start rule:oncall

### Без раздела Rules (резерв `session_start`/`recovery` вычтен в обоих плечах; §17 п. 2, решение 10)

| Часть | база | плечо | R | session_start R | recovery R |
|---|---:|---:|---|---|---|
| всего (8) | 13529 | 14446 | 1.068 [1.042–1.106] | 1.385 [1.263–1.885] | — |
| standard (6) | 5218 | 5617 | 1.077 [1.029–1.138] | 1.727 [1.315–2.630] | — |
| heavy (2) | 8311 | 8828 | 1.062 [1.045–1.086] | 1.255 [1.251–1.259] | — |

- |Δ| / база токенов сессии, 95-й перцентиль: 0.1279
- бутстреп: 10000 выборок, зерно 0x5eed20261019 (парный по сессиям; токены сессии — среднее по повторам).
- повторная выдача — справочно и смещена в пользу M6: повтор `/read` с пометкой «уже выдавалось» считается помеченным, у плеча без пометки (до M6) тот же повтор — неотмеченным.
