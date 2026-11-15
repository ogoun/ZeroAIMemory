
## ТС-2: 2026-11-15-m6-bge-m3-fp16-sessions-t13b-b57-claude-a1-srv0.0.10-m5.7-holdout (0.0.10-m5.7) против 2026-11-15-m6-bge-m3-fp16-sessions-t13b-b56-claude-a1-srv0.0.9-m5.6-holdout (0.0.9-m5.6)

- исключено из обоих плеч: события {"session_start_after_abort"}; классы строк {}; обязательные строки {"idea"}; ожидания {"idea"}

### всего (сессий 8)

| Событие | база | плечо | R |
|---|---:|---:|---|
| checkpoint | 0 | 0 | — |
| file_context | 0 | 0 | — |
| post_tool | 0 | 0 | — |
| pre_tool | 547 | 547 | 1.000 [1.000–1.000] |
| prompt | 1494 | 1599 | 1.070 [0.872–1.114] |
| question | 5996 | 5998 | 1.000 [1.000–1.001] |
| repeat | 1438 | 1438 | 1.000 [1.000–1.001] |
| session_start | 4285 | 4854 | 1.133 [1.053–1.301] |
| subagent_mcp | 1242 | 1243 | 1.001 [1.000–1.001] |
| subagent_start | 825 | 825 | 1.000 [1.000–1.000] |
| всего | 15827 | 16504 | 1.043 [1.025–1.076] |

- Δ (плечо − база, общие элементы, доли сессий — среднее по повторам, односторонние 95 %): hint_recall 0.000 [0.000–0.000], строго 0.000 [0.000–0.000], обязательные строки -0.042 [-0.071–0.000]
- **база**: сессий 8 × 3 повторов, токенов 47480 (checkpoint 0, file_context 0, post_tool 0, pre_tool 1641, prompt 4482, question 17989, repeat 4313, session_start 12855, subagent_mcp 3725, subagent_start 2475); обязательные строки 184/213 (0.864); нагеты вопросов 33/33 (1.000); дайджест при двух стартах 24/24 (1.000)
  - hint_recall общий 87/87 (1.000) (строго в событии 36/87 (0.414)); подсказки-идеи — (строго —); по видам: insight 30/30 (1.000) (строго 30/30 (1.000)); rule 48/48 (1.000) (строго 0/48 (0.000)); term 9/9 (1.000) (строго 6/9 (0.667)); зачёт: digest 48, event 36, hooks 3
  - повторная выдача 102/520 (0.196) (между каналами 238/493 (0.483)); идеи: 0; снимок сессии: 0; раздел Rules: session_start 6861; шум повторов (p95 |Δ|/среднее сессии): 0.0045
  - обязательные строки, худший повтор: r1 61/71 (0.859)
  - нет строк: sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:oncall
- **плечо**: сессий 8 × 3 повторов, токенов 49512 (checkpoint 0, file_context 0, post_tool 0, pre_tool 1641, prompt 4797, question 17993, repeat 4315, session_start 14563, subagent_mcp 3728, subagent_start 2475); обязательные строки 175/213 (0.822); нагеты вопросов 33/33 (1.000); дайджест при двух стартах 24/24 (1.000)
  - hint_recall общий 87/87 (1.000) (строго в событии 36/87 (0.414)); подсказки-идеи — (строго —); по видам: insight 30/30 (1.000) (строго 30/30 (1.000)); rule 48/48 (1.000) (строго 0/48 (0.000)); term 9/9 (1.000) (строго 6/9 (0.667)); зачёт: digest 48, event 36, hooks 3
  - повторная выдача 101/558 (0.181) (между каналами 233/531 (0.439)); идеи: prompt 552, session_start 2217; снимок сессии: 0; раздел Rules: session_start 6287; шум повторов (p95 |Δ|/среднее сессии): 0.0273
  - обязательные строки, худший повтор: r0 58/71 (0.817)
  - нет строк: sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:frontend-a11y; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:i18n; sh-02 session_start rule:cache-headers; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:frontend-a11y; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:i18n; sh-02 session_start rule:cache-headers; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:i18n; sh-02 session_start rule:cache-headers; sh-02 session_start rule:oncall

### standard (сессий 6)

| Событие | база | плечо | R |
|---|---:|---:|---|
| checkpoint | 0 | 0 | — |
| file_context | 0 | 0 | — |
| post_tool | 0 | 0 | — |
| pre_tool | 368 | 368 | 1.000 [1.000–1.000] |
| prompt | 367 | 338 | 0.921 [0.760–1.000] |
| question | 3015 | 3014 | 1.000 [0.999–1.000] |
| repeat | 485 | 485 | 1.000 [1.000–1.000] |
| session_start | 1558 | 1957 | 1.256 [1.120–1.388] |
| subagent_mcp | 269 | 269 | 1.000 [1.000–1.000] |
| subagent_start | 167 | 167 | 1.000 [1.000–1.000] |
| всего | 6228 | 6598 | 1.059 [1.021–1.105] |

- Δ (плечо − база, общие элементы, доли сессий — среднее по повторам, односторонние 95 %): hint_recall 0.000 [0.000–0.000], строго 0.000 [0.000–0.000], обязательные строки 0.000 [0.000–0.000]
- **база**: сессий 6 × 3 повторов, токенов 18685 (checkpoint 0, file_context 0, post_tool 0, pre_tool 1104, prompt 1101, question 9044, repeat 1455, session_start 4674, subagent_mcp 806, subagent_start 501); обязательные строки 69/69 (1.000); нагеты вопросов 21/21 (1.000); дайджест при двух стартах 18/18 (1.000)
  - hint_recall общий 60/60 (1.000) (строго в событии 18/60 (0.300)); подсказки-идеи — (строго —); по видам: insight 18/18 (1.000) (строго 18/18 (1.000)); rule 42/42 (1.000) (строго 0/42 (0.000)); зачёт: digest 42, event 18
  - повторная выдача 36/201 (0.179) (между каналами 105/192 (0.547)); идеи: 0; снимок сессии: 0; раздел Rules: session_start 3027; шум повторов (p95 |Δ|/среднее сессии): 0.0007
  - обязательные строки, худший повтор: r0 23/23 (1.000)
- **плечо**: сессий 6 × 3 повторов, токенов 19793 (checkpoint 0, file_context 0, post_tool 0, pre_tool 1104, prompt 1014, question 9042, repeat 1455, session_start 5871, subagent_mcp 806, subagent_start 501); обязательные строки 69/69 (1.000); нагеты вопросов 21/21 (1.000); дайджест при двух стартах 18/18 (1.000)
  - hint_recall общий 60/60 (1.000) (строго в событии 18/60 (0.300)); подсказки-идеи — (строго —); по видам: insight 18/18 (1.000) (строго 18/18 (1.000)); rule 42/42 (1.000) (строго 0/42 (0.000)); зачёт: digest 42, event 18
  - повторная выдача 35/218 (0.161) (между каналами 104/209 (0.498)); идеи: session_start 1209; снимок сессии: 0; раздел Rules: session_start 3027; шум повторов (p95 |Δ|/среднее сессии): 0.0314
  - обязательные строки, худший повтор: r0 23/23 (1.000)

### heavy (сессий 2)

| Событие | база | плечо | R |
|---|---:|---:|---|
| checkpoint | 0 | 0 | — |
| file_context | 0 | 0 | — |
| post_tool | 0 | 0 | — |
| pre_tool | 179 | 179 | 1.000 [1.000–1.000] |
| prompt | 1127 | 1261 | 1.119 [1.110–1.134] |
| question | 2982 | 2984 | 1.001 [1.000–1.001] |
| repeat | 953 | 953 | 1.001 [1.001–1.001] |
| session_start | 2727 | 2897 | 1.062 [1.006–1.128] |
| subagent_mcp | 973 | 974 | 1.001 [1.001–1.001] |
| subagent_start | 658 | 658 | 1.000 [1.000–1.000] |
| всего | 9598 | 9906 | 1.032 [1.021–1.041] |

- Δ (плечо − база, общие элементы, доли сессий — среднее по повторам, односторонние 95 %): hint_recall 0.000 [0.000–0.000], строго 0.000 [0.000–0.000], обязательные строки -0.062 [-0.094–0.000]
- **база**: сессий 2 × 3 повторов, токенов 28795 (checkpoint 0, file_context 0, post_tool 0, pre_tool 537, prompt 3381, question 8945, repeat 2858, session_start 8181, subagent_mcp 2919, subagent_start 1974); обязательные строки 115/144 (0.799); нагеты вопросов 12/12 (1.000); дайджест при двух стартах 6/6 (1.000)
  - hint_recall общий 27/27 (1.000) (строго в событии 18/27 (0.667)); подсказки-идеи — (строго —); по видам: insight 12/12 (1.000) (строго 12/12 (1.000)); rule 6/6 (1.000) (строго 0/6 (0.000)); term 9/9 (1.000) (строго 6/9 (0.667)); зачёт: digest 6, event 18, hooks 3
  - повторная выдача 66/319 (0.207) (между каналами 133/301 (0.442)); идеи: 0; снимок сессии: 0; раздел Rules: session_start 3834; шум повторов (p95 |Δ|/среднее сессии): 0.0077
  - обязательные строки, худший повтор: r1 38/48 (0.792)
  - нет строк: sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:oncall
- **плечо**: сессий 2 × 3 повторов, токенов 29719 (checkpoint 0, file_context 0, post_tool 0, pre_tool 537, prompt 3783, question 8951, repeat 2860, session_start 8692, subagent_mcp 2922, subagent_start 1974); обязательные строки 106/144 (0.736); нагеты вопросов 12/12 (1.000); дайджест при двух стартах 6/6 (1.000)
  - hint_recall общий 27/27 (1.000) (строго в событии 18/27 (0.667)); подсказки-идеи — (строго —); по видам: insight 12/12 (1.000) (строго 12/12 (1.000)); rule 6/6 (1.000) (строго 0/6 (0.000)); term 9/9 (1.000) (строго 6/9 (0.667)); зачёт: digest 6, event 18, hooks 3
  - повторная выдача 66/340 (0.194) (между каналами 129/322 (0.401)); идеи: prompt 552, session_start 1008; снимок сессии: 0; раздел Rules: session_start 3260; шум повторов (p95 |Δ|/среднее сессии): 0.0060
  - обязательные строки, худший повтор: r0 35/48 (0.729)
  - нет строк: sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:frontend-a11y; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:i18n; sh-02 session_start rule:cache-headers; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:frontend-a11y; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:i18n; sh-02 session_start rule:cache-headers; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:i18n; sh-02 session_start rule:cache-headers; sh-02 session_start rule:oncall

### Без раздела Rules (резерв `session_start`/`recovery` вычтен в обоих плечах; §17 п. 2, решение 10)

| Часть | база | плечо | R | session_start R | recovery R |
|---|---:|---:|---|---|---|
| всего (8) | 13540 | 14408 | 1.064 [1.039–1.100] | 1.381 [1.257–1.885] | — |
| standard (6) | 5219 | 5589 | 1.071 [1.024–1.131] | 1.727 [1.315–2.630] | — |
| heavy (2) | 8320 | 8820 | 1.060 [1.046–1.080] | 1.250 [1.249–1.251] | — |

- |Δ| / база токенов сессии, 95-й перцентиль: 0.1278
- бутстреп: 10000 выборок, зерно 0x5eed20261019 (парный по сессиям; токены сессии — среднее по повторам).
- повторная выдача — справочно и смещена в пользу M6: повтор `/read` с пометкой «уже выдавалось» считается помеченным, у плеча без пометки (до M6) тот же повтор — неотмеченным.
