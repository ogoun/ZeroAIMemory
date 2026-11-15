
## ТС-2: 2026-11-15-m6-bge-m3-fp16-sessions-t13b-m6-claude-a2-srv0.0.10-m5.7+m6-holdout (0.0.10-m5.7+m6) против 2026-11-15-m6-bge-m3-fp16-sessions-t13b-b56-claude-a2-srv0.0.9-m5.6-holdout (0.0.9-m5.6)

- исключено из обоих плеч: события {"session_start_after_abort"}; классы строк {}; обязательные строки {"idea"}; ожидания {"idea"}

### всего (сессий 8)

| Событие | база | плечо | R |
|---|---:|---:|---|
| checkpoint | 0 | 0 | — |
| file_context | 0 | 0 | — |
| post_tool | 0 | 0 | — |
| pre_tool | 547 | 538 | 0.984 [0.983–0.984] |
| prompt | 1494 | 1542 | 1.032 [0.989–1.090] |
| question | 5996 | 5513 | 0.919 [0.865–0.975] |
| repeat | 1438 | 1280 | 0.891 [0.863–0.944] |
| session_start | 4305 | 6646 | 1.544 [1.318–1.766] |
| subagent_mcp | 1242 | 998 | 0.804 [0.744–1.021] |
| subagent_start | 825 | 720 | 0.873 [0.856–0.877] |
| всего | 15846 | 17237 | 1.088 [0.983–1.215] |

- Δ (плечо − база, общие элементы, доли сессий — среднее по повторам, односторонние 95 %): hint_recall 0.000 [0.000–0.000], строго 0.000 [0.000–0.000], обязательные строки 0.127 [0.000–0.212]
- **база**: сессий 8 × 3 повторов, токенов 47539 (checkpoint 0, file_context 0, post_tool 0, pre_tool 1641, prompt 4482, question 17988, repeat 4313, session_start 12915, subagent_mcp 3725, subagent_start 2475); обязательные строки 186/213 (0.873); нагеты вопросов 33/33 (1.000); дайджест при двух стартах 24/24 (1.000)
  - hint_recall общий 87/87 (1.000) (строго в событии 36/87 (0.414)); подсказки-идеи — (строго —); по видам: insight 30/30 (1.000) (строго 30/30 (1.000)); rule 48/48 (1.000) (строго 0/48 (0.000)); term 9/9 (1.000) (строго 6/9 (0.667)); зачёт: digest 48, event 36, hooks 3
  - повторная выдача 102/522 (0.195) (между каналами 240/495 (0.485)); идеи: 0; снимок сессии: 0; раздел Rules: session_start 6969; шум повторов (p95 |Δ|/среднее сессии): 0.0007
  - обязательные строки, худший повтор: r0 62/71 (0.873)
  - нет строк: sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:oncall
- **плечо**: сессий 8 × 3 повторов, токенов 51712 (checkpoint 0, file_context 0, post_tool 0, pre_tool 1614, prompt 4626, question 16538, repeat 3841, session_start 19938, subagent_mcp 2995, subagent_start 2160); обязательные строки 213/213 (1.000); нагеты вопросов 33/33 (1.000); дайджест при двух стартах 24/24 (1.000)
  - hint_recall общий 87/87 (1.000) (строго в событии 36/87 (0.414)); подсказки-идеи — (строго —); по видам: insight 30/30 (1.000) (строго 30/30 (1.000)); rule 48/48 (1.000) (строго 0/48 (0.000)); term 9/9 (1.000) (строго 6/9 (0.667)); зачёт: digest 48, event 36, hooks 3
  - повторная выдача 93/551 (0.169) (между каналами 221/539 (0.410)); идеи: prompt 552, session_start 2217; снимок сессии: 0; раздел Rules: session_start 10965; шум повторов (p95 |Δ|/среднее сессии): 0.0089
  - обязательные строки, худший повтор: r0 71/71 (1.000)

### standard (сессий 6)

| Событие | база | плечо | R |
|---|---:|---:|---|
| checkpoint | 0 | 0 | — |
| file_context | 0 | 0 | — |
| post_tool | 0 | 0 | — |
| pre_tool | 368 | 362 | 0.984 [0.983–0.984] |
| prompt | 367 | 363 | 0.989 [0.987–0.990] |
| question | 3014 | 2923 | 0.970 [0.960–0.978] |
| repeat | 485 | 458 | 0.944 [0.944–0.944] |
| session_start | 1558 | 2249 | 1.444 [1.329–1.605] |
| subagent_mcp | 268 | 274 | 1.021 [1.021–1.021] |
| subagent_start | 167 | 143 | 0.856 [0.856–0.856] |
| всего | 6228 | 6772 | 1.087 [1.043–1.143] |

- Δ (плечо − база, общие элементы, доли сессий — среднее по повторам, односторонние 95 %): hint_recall 0.000 [0.000–0.000], строго 0.000 [0.000–0.000], обязательные строки 0.000 [0.000–0.000]
- **база**: сессий 6 × 3 повторов, токенов 18683 (checkpoint 0, file_context 0, post_tool 0, pre_tool 1104, prompt 1101, question 9042, repeat 1456, session_start 4674, subagent_mcp 805, subagent_start 501); обязательные строки 69/69 (1.000); нагеты вопросов 21/21 (1.000); дайджест при двух стартах 18/18 (1.000)
  - hint_recall общий 60/60 (1.000) (строго в событии 18/60 (0.300)); подсказки-идеи — (строго —); по видам: insight 18/18 (1.000) (строго 18/18 (1.000)); rule 42/42 (1.000) (строго 0/42 (0.000)); зачёт: digest 42, event 18
  - повторная выдача 36/201 (0.179) (между каналами 105/192 (0.547)); идеи: 0; снимок сессии: 0; раздел Rules: session_start 3027; шум повторов (p95 |Δ|/среднее сессии): 0.0008
  - обязательные строки, худший повтор: r0 23/23 (1.000)
- **плечо**: сессий 6 × 3 повторов, токенов 20317 (checkpoint 0, file_context 0, post_tool 0, pre_tool 1086, prompt 1089, question 8770, repeat 1374, session_start 6747, subagent_mcp 822, subagent_start 429); обязательные строки 69/69 (1.000); нагеты вопросов 21/21 (1.000); дайджест при двух стартах 18/18 (1.000)
  - hint_recall общий 60/60 (1.000) (строго в событии 18/60 (0.300)); подсказки-идеи — (строго —); по видам: insight 18/18 (1.000) (строго 18/18 (1.000)); rule 42/42 (1.000) (строго 0/42 (0.000)); зачёт: digest 42, event 18
  - повторная выдача 33/213 (0.155) (между каналами 102/207 (0.493)); идеи: session_start 1209; снимок сессии: 0; раздел Rules: session_start 3909; шум повторов (p95 |Δ|/среднее сессии): 0.0007
  - обязательные строки, худший повтор: r0 23/23 (1.000)

### heavy (сессий 2)

| Событие | база | плечо | R |
|---|---:|---:|---|
| checkpoint | 0 | 0 | — |
| file_context | 0 | 0 | — |
| post_tool | 0 | 0 | — |
| pre_tool | 179 | 176 | 0.983 [0.982–0.984] |
| prompt | 1127 | 1179 | 1.046 [1.006–1.113] |
| question | 2982 | 2589 | 0.868 [0.823–0.971] |
| repeat | 952 | 822 | 0.863 [0.863–0.863] |
| session_start | 2747 | 4397 | 1.601 [1.285–1.868] |
| subagent_mcp | 973 | 724 | 0.744 [0.744–0.744] |
| subagent_start | 658 | 577 | 0.877 [0.877–0.877] |
| всего | 9619 | 10465 | 1.088 [0.947–1.272] |

- Δ (плечо − база, общие элементы, доли сессий — среднее по повторам, односторонние 95 %): hint_recall 0.000 [0.000–0.000], строго 0.000 [0.000–0.000], обязательные строки 0.188 [0.000–0.281]
- **база**: сессий 2 × 3 повторов, токенов 28856 (checkpoint 0, file_context 0, post_tool 0, pre_tool 537, prompt 3381, question 8946, repeat 2857, session_start 8241, subagent_mcp 2920, subagent_start 1974); обязательные строки 117/144 (0.812); нагеты вопросов 12/12 (1.000); дайджест при двух стартах 6/6 (1.000)
  - hint_recall общий 27/27 (1.000) (строго в событии 18/27 (0.667)); подсказки-идеи — (строго —); по видам: insight 12/12 (1.000) (строго 12/12 (1.000)); rule 6/6 (1.000) (строго 0/6 (0.000)); term 9/9 (1.000) (строго 6/9 (0.667)); зачёт: digest 6, event 18, hooks 3
  - повторная выдача 66/321 (0.206) (между каналами 135/303 (0.446)); идеи: 0; снимок сессии: 0; раздел Rules: session_start 3942; шум повторов (p95 |Δ|/среднее сессии): 0.0003
  - обязательные строки, худший повтор: r0 39/48 (0.812)
  - нет строк: sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:oncall
- **плечо**: сессий 2 × 3 повторов, токенов 31395 (checkpoint 0, file_context 0, post_tool 0, pre_tool 528, prompt 3537, question 7768, repeat 2467, session_start 13191, subagent_mcp 2173, subagent_start 1731); обязательные строки 144/144 (1.000); нагеты вопросов 12/12 (1.000); дайджест при двух стартах 6/6 (1.000)
  - hint_recall общий 27/27 (1.000) (строго в событии 18/27 (0.667)); подсказки-идеи — (строго —); по видам: insight 12/12 (1.000) (строго 12/12 (1.000)); rule 6/6 (1.000) (строго 0/6 (0.000)); term 9/9 (1.000) (строго 6/9 (0.667)); зачёт: digest 6, event 18, hooks 3
  - повторная выдача 60/338 (0.178) (между каналами 119/332 (0.358)); идеи: prompt 552, session_start 1008; снимок сессии: 0; раздел Rules: session_start 7056; шум повторов (p95 |Δ|/среднее сессии): 0.0126
  - обязательные строки, худший повтор: r0 48/48 (1.000)

### Без раздела Rules (резерв `session_start`/`recovery` вычтен в обоих плечах; §17 п. 2, решение 10)

| Часть | база | плечо | R | session_start R | recovery R |
|---|---:|---:|---|---|---|
| всего (8) | 13523 | 13582 | 1.004 [0.935–1.090] | 1.509 [1.307–1.900] | — |
| standard (6) | 5219 | 5469 | 1.048 [0.994–1.111] | 1.723 [1.312–2.630] | — |
| heavy (2) | 8305 | 8113 | 0.977 [0.899–1.085] | 1.427 [1.249–1.573] | — |

- |Δ| / база токенов сессии, 95-й перцентиль: 0.2481
- бутстреп: 10000 выборок, зерно 0x5eed20261019 (парный по сессиям; токены сессии — среднее по повторам).
- повторная выдача — справочно и смещена в пользу M6: повтор `/read` с пометкой «уже выдавалось» считается помеченным, у плеча без пометки (до M6) тот же повтор — неотмеченным.
