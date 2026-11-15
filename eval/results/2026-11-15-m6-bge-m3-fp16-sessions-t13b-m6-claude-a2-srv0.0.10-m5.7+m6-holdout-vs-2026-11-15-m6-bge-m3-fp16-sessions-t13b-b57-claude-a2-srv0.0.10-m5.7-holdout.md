
## ТС-2: 2026-11-15-m6-bge-m3-fp16-sessions-t13b-m6-claude-a2-srv0.0.10-m5.7+m6-holdout (0.0.10-m5.7+m6) против 2026-11-15-m6-bge-m3-fp16-sessions-t13b-b57-claude-a2-srv0.0.10-m5.7-holdout (0.0.10-m5.7)

- исключено из обоих плеч: события {"session_start_after_abort"}; классы строк {}; обязательные строки {}; ожидания {}

### всего (сессий 8)

| Событие | база | плечо | R |
|---|---:|---:|---|
| checkpoint | 0 | 0 | — |
| file_context | 0 | 0 | — |
| post_tool | 0 | 0 | — |
| pre_tool | 547 | 538 | 0.984 [0.983–0.984] |
| prompt | 1628 | 1542 | 0.947 [0.916–0.990] |
| question | 5998 | 5513 | 0.919 [0.865–0.974] |
| repeat | 1439 | 1280 | 0.890 [0.862–0.943] |
| session_start | 4854 | 6646 | 1.369 [1.113–1.666] |
| subagent_mcp | 1242 | 998 | 0.804 [0.744–1.021] |
| subagent_start | 825 | 720 | 0.873 [0.856–0.877] |
| всего | 16534 | 17237 | 1.043 [0.938–1.174] |

- Δ (плечо − база, общие элементы, доли сессий — среднее по повторам, односторонние 95 %): hint_recall 0.000 [0.000–0.000], строго 0.000 [0.000–0.000], обязательные строки 0.153 [0.000–0.266]
- **база**: сессий 8 × 3 повторов, токенов 49602 (checkpoint 0, file_context 0, post_tool 0, pre_tool 1641, prompt 4884, question 17995, repeat 4318, session_start 14563, subagent_mcp 3726, subagent_start 2475); обязательные строки 211/249 (0.847); нагеты вопросов 33/33 (1.000); дайджест при двух стартах 24/24 (1.000)
  - hint_recall общий 87/87 (1.000) (строго в событии 36/87 (0.414)); подсказки-идеи 21/21 (1.000) (строго 9/21 (0.429)); по видам: idea 21/21 (1.000) (строго 9/21 (0.429)); insight 30/30 (1.000) (строго 30/30 (1.000)); rule 48/48 (1.000) (строго 0/48 (0.000)); term 9/9 (1.000) (строго 6/9 (0.667)); зачёт: digest 60, event 45, hooks 3
  - повторная выдача 102/559 (0.182) (между каналами 234/532 (0.440)); идеи: prompt 552, session_start 2217; снимок сессии: 0; раздел Rules: session_start 6287; шум повторов (p95 |Δ|/среднее сессии): 0.0036
  - обязательные строки, худший повтор: r0 70/83 (0.843)
  - нет строк: sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:frontend-a11y; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:i18n; sh-02 session_start rule:cache-headers; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:i18n; sh-02 session_start rule:cache-headers; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:frontend-a11y; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:i18n; sh-02 session_start rule:cache-headers; sh-02 session_start rule:oncall
- **плечо**: сессий 8 × 3 повторов, токенов 51712 (checkpoint 0, file_context 0, post_tool 0, pre_tool 1614, prompt 4626, question 16538, repeat 3841, session_start 19938, subagent_mcp 2995, subagent_start 2160); обязательные строки 249/249 (1.000); нагеты вопросов 33/33 (1.000); дайджест при двух стартах 24/24 (1.000)
  - hint_recall общий 87/87 (1.000) (строго в событии 36/87 (0.414)); подсказки-идеи 21/21 (1.000) (строго 9/21 (0.429)); по видам: idea 21/21 (1.000) (строго 9/21 (0.429)); insight 30/30 (1.000) (строго 30/30 (1.000)); rule 48/48 (1.000) (строго 0/48 (0.000)); term 9/9 (1.000) (строго 6/9 (0.667)); зачёт: digest 60, event 45, hooks 3
  - повторная выдача 93/551 (0.169) (между каналами 221/539 (0.410)); идеи: prompt 552, session_start 2217; снимок сессии: 0; раздел Rules: session_start 10965; шум повторов (p95 |Δ|/среднее сессии): 0.0089
  - обязательные строки, худший повтор: r0 83/83 (1.000)

### standard (сессий 6)

| Событие | база | плечо | R |
|---|---:|---:|---|
| checkpoint | 0 | 0 | — |
| file_context | 0 | 0 | — |
| post_tool | 0 | 0 | — |
| pre_tool | 368 | 362 | 0.984 [0.983–0.984] |
| prompt | 367 | 363 | 0.989 [0.987–0.990] |
| question | 3015 | 2923 | 0.969 [0.960–0.978] |
| repeat | 486 | 458 | 0.943 [0.943–0.943] |
| session_start | 1957 | 2249 | 1.149 [1.091–1.234] |
| subagent_mcp | 268 | 274 | 1.021 [1.021–1.021] |
| subagent_start | 167 | 143 | 0.856 [0.856–0.856] |
| всего | 6628 | 6772 | 1.022 [1.007–1.046] |

- Δ (плечо − база, общие элементы, доли сессий — среднее по повторам, односторонние 95 %): hint_recall 0.000 [0.000–0.000], строго 0.000 [0.000–0.000], обязательные строки 0.000 [0.000–0.000]
- **база**: сессий 6 × 3 повторов, токенов 19885 (checkpoint 0, file_context 0, post_tool 0, pre_tool 1104, prompt 1101, question 9046, repeat 1457, session_start 5871, subagent_mcp 805, subagent_start 501); обязательные строки 87/87 (1.000); нагеты вопросов 21/21 (1.000); дайджест при двух стартах 18/18 (1.000)
  - hint_recall общий 60/60 (1.000) (строго в событии 18/60 (0.300)); подсказки-идеи 12/12 (1.000) (строго 0/12 (0.000)); по видам: idea 12/12 (1.000) (строго 0/12 (0.000)); insight 18/18 (1.000) (строго 18/18 (1.000)); rule 42/42 (1.000) (строго 0/42 (0.000)); зачёт: digest 54, event 18
  - повторная выдача 36/219 (0.164) (между каналами 105/210 (0.500)); идеи: session_start 1209; снимок сессии: 0; раздел Rules: session_start 3027; шум повторов (p95 |Δ|/среднее сессии): 0.0006
  - обязательные строки, худший повтор: r0 29/29 (1.000)
- **плечо**: сессий 6 × 3 повторов, токенов 20317 (checkpoint 0, file_context 0, post_tool 0, pre_tool 1086, prompt 1089, question 8770, repeat 1374, session_start 6747, subagent_mcp 822, subagent_start 429); обязательные строки 87/87 (1.000); нагеты вопросов 21/21 (1.000); дайджест при двух стартах 18/18 (1.000)
  - hint_recall общий 60/60 (1.000) (строго в событии 18/60 (0.300)); подсказки-идеи 12/12 (1.000) (строго 0/12 (0.000)); по видам: idea 12/12 (1.000) (строго 0/12 (0.000)); insight 18/18 (1.000) (строго 18/18 (1.000)); rule 42/42 (1.000) (строго 0/42 (0.000)); зачёт: digest 54, event 18
  - повторная выдача 33/213 (0.155) (между каналами 102/207 (0.493)); идеи: session_start 1209; снимок сессии: 0; раздел Rules: session_start 3909; шум повторов (p95 |Δ|/среднее сессии): 0.0007
  - обязательные строки, худший повтор: r0 29/29 (1.000)

### heavy (сессий 2)

| Событие | база | плечо | R |
|---|---:|---:|---|
| checkpoint | 0 | 0 | — |
| file_context | 0 | 0 | — |
| post_tool | 0 | 0 | — |
| pre_tool | 179 | 176 | 0.983 [0.982–0.984] |
| prompt | 1261 | 1179 | 0.935 [0.906–0.981] |
| question | 2983 | 2589 | 0.868 [0.823–0.970] |
| repeat | 954 | 822 | 0.862 [0.862–0.862] |
| session_start | 2897 | 4397 | 1.518 [1.139–1.883] |
| subagent_mcp | 974 | 724 | 0.744 [0.744–0.744] |
| subagent_start | 658 | 577 | 0.877 [0.877–0.877] |
| всего | 9906 | 10465 | 1.056 [0.910–1.251] |

- Δ (плечо − база, общие элементы, доли сессий — среднее по повторам, односторонние 95 %): hint_recall 0.000 [0.000–0.000], строго 0.000 [0.000–0.000], обязательные строки 0.235 [0.000–0.362]
- **база**: сессий 2 × 3 повторов, токенов 29717 (checkpoint 0, file_context 0, post_tool 0, pre_tool 537, prompt 3783, question 8949, repeat 2861, session_start 8692, subagent_mcp 2921, subagent_start 1974); обязательные строки 124/162 (0.765); нагеты вопросов 12/12 (1.000); дайджест при двух стартах 6/6 (1.000)
  - hint_recall общий 27/27 (1.000) (строго в событии 18/27 (0.667)); подсказки-идеи 9/9 (1.000) (строго 9/9 (1.000)); по видам: idea 9/9 (1.000) (строго 9/9 (1.000)); insight 12/12 (1.000) (строго 12/12 (1.000)); rule 6/6 (1.000) (строго 0/6 (0.000)); term 9/9 (1.000) (строго 6/9 (0.667)); зачёт: digest 6, event 27, hooks 3
  - повторная выдача 66/340 (0.194) (между каналами 129/322 (0.401)); идеи: prompt 552, session_start 1008; снимок сессии: 0; раздел Rules: session_start 3260; шум повторов (p95 |Δ|/среднее сессии): 0.0061
  - обязательные строки, худший повтор: r0 41/54 (0.759)
  - нет строк: sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:frontend-a11y; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:i18n; sh-02 session_start rule:cache-headers; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:i18n; sh-02 session_start rule:cache-headers; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:frontend-a11y; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:i18n; sh-02 session_start rule:cache-headers; sh-02 session_start rule:oncall
- **плечо**: сессий 2 × 3 повторов, токенов 31395 (checkpoint 0, file_context 0, post_tool 0, pre_tool 528, prompt 3537, question 7768, repeat 2467, session_start 13191, subagent_mcp 2173, subagent_start 1731); обязательные строки 162/162 (1.000); нагеты вопросов 12/12 (1.000); дайджест при двух стартах 6/6 (1.000)
  - hint_recall общий 27/27 (1.000) (строго в событии 18/27 (0.667)); подсказки-идеи 9/9 (1.000) (строго 9/9 (1.000)); по видам: idea 9/9 (1.000) (строго 9/9 (1.000)); insight 12/12 (1.000) (строго 12/12 (1.000)); rule 6/6 (1.000) (строго 0/6 (0.000)); term 9/9 (1.000) (строго 6/9 (0.667)); зачёт: digest 6, event 27, hooks 3
  - повторная выдача 60/338 (0.178) (между каналами 119/332 (0.358)); идеи: prompt 552, session_start 1008; снимок сессии: 0; раздел Rules: session_start 7056; шум повторов (p95 |Δ|/среднее сессии): 0.0126
  - обязательные строки, худший повтор: r0 54/54 (1.000)

### Без раздела Rules (резерв `session_start`/`recovery` вычтен в обоих плечах; §17 п. 2, решение 10)

| Часть | база | плечо | R | session_start R | recovery R |
|---|---:|---:|---|---|---|
| всего (8) | 14438 | 13582 | 0.941 [0.888–0.992] | 1.084 [0.997–1.181] | — |
| standard (6) | 5619 | 5469 | 0.973 [0.963–0.983] | 0.998 [0.997–1.000] | — |
| heavy (2) | 8819 | 8113 | 0.920 [0.860–1.000] | 1.129 [0.999–1.235] | — |

- подсказки-идеи prompt совпали с базой: 24/24 (1.000)
- |Δ| / база токенов сессии, 95-й перцентиль: 0.1949
- бутстреп: 10000 выборок, зерно 0x5eed20261019 (парный по сессиям; токены сессии — среднее по повторам).
- повторная выдача — справочно и смещена в пользу M6: повтор `/read` с пометкой «уже выдавалось» считается помеченным, у плеча без пометки (до M6) тот же повтор — неотмеченным.
