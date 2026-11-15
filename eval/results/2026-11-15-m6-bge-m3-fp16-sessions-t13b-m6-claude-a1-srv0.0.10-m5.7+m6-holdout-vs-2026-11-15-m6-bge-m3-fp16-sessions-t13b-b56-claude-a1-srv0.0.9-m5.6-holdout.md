
## ТС-2: 2026-11-15-m6-bge-m3-fp16-sessions-t13b-m6-claude-a1-srv0.0.10-m5.7+m6-holdout (0.0.10-m5.7+m6) против 2026-11-15-m6-bge-m3-fp16-sessions-t13b-b56-claude-a1-srv0.0.9-m5.6-holdout (0.0.9-m5.6)

- исключено из обоих плеч: события {"session_start_after_abort"}; классы строк {}; обязательные строки {"idea"}; ожидания {"idea"}

### всего (сессий 8)

| Событие | база | плечо | R |
|---|---:|---:|---|
| checkpoint | 0 | 0 | — |
| file_context | 0 | 0 | — |
| post_tool | 0 | 0 | — |
| pre_tool | 547 | 538 | 0.984 [0.983–0.984] |
| prompt | 1494 | 1206 | 0.807 [0.523–0.990] |
| question | 5996 | 5547 | 0.925 [0.878–0.975] |
| repeat | 1438 | 1280 | 0.891 [0.863–0.944] |
| session_start | 4285 | 6646 | 1.551 [1.318–1.782] |
| subagent_mcp | 1242 | 999 | 0.805 [0.745–1.020] |
| subagent_start | 825 | 681 | 0.825 [0.817–0.856] |
| всего | 15827 | 16897 | 1.068 [0.942–1.216] |

- Δ (плечо − база, общие элементы, доли сессий — среднее по повторам, односторонние 95 %): hint_recall -0.023 [-0.061–0.000], строго -0.023 [-0.061–0.000], обязательные строки 0.136 [0.000–0.227]
- **база**: сессий 8 × 3 повторов, токенов 47480 (checkpoint 0, file_context 0, post_tool 0, pre_tool 1641, prompt 4482, question 17989, repeat 4313, session_start 12855, subagent_mcp 3725, subagent_start 2475); обязательные строки 184/213 (0.864); нагеты вопросов 33/33 (1.000); дайджест при двух стартах 24/24 (1.000)
  - hint_recall общий 87/87 (1.000) (строго в событии 36/87 (0.414)); подсказки-идеи — (строго —); по видам: insight 30/30 (1.000) (строго 30/30 (1.000)); rule 48/48 (1.000) (строго 0/48 (0.000)); term 9/9 (1.000) (строго 6/9 (0.667)); зачёт: digest 48, event 36, hooks 3
  - повторная выдача 102/520 (0.196) (между каналами 238/493 (0.483)); идеи: 0; снимок сессии: 0; раздел Rules: session_start 6861; шум повторов (p95 |Δ|/среднее сессии): 0.0045
  - обязательные строки, худший повтор: r1 61/71 (0.859)
  - нет строк: sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:oncall
- **плечо**: сессий 8 × 3 повторов, токенов 50692 (checkpoint 0, file_context 0, post_tool 0, pre_tool 1614, prompt 3618, question 16642, repeat 3841, session_start 19938, subagent_mcp 2997, subagent_start 2042); обязательные строки 213/213 (1.000); нагеты вопросов 33/33 (1.000); дайджест при двух стартах 24/24 (1.000)
  - hint_recall общий 85/87 (0.977) (строго в событии 34/87 (0.391)); подсказки-идеи — (строго —); по видам: insight 28/30 (0.933) (строго 28/30 (0.933)); rule 48/48 (1.000) (строго 0/48 (0.000)); term 9/9 (1.000) (строго 6/9 (0.667)); зачёт: digest 48, event 34, hooks 3
  - повторная выдача 87/541 (0.161) (между каналами 217/531 (0.409)); идеи: prompt 438, session_start 2217; снимок сессии: 0; раздел Rules: session_start 10965; шум повторов (p95 |Δ|/среднее сессии): 0.0562; неразобранных выводов хука: 0, хуков сверх бюджета: 1
  - обязательные строки, худший повтор: r0 71/71 (1.000)
  - повторы с промахом бюджета хука или неразобранным выводом (действительны; блокируют приёмку 13b): ss-10 r0

### standard (сессий 6)

| Событие | база | плечо | R |
|---|---:|---:|---|
| checkpoint | 0 | 0 | — |
| file_context | 0 | 0 | — |
| post_tool | 0 | 0 | — |
| pre_tool | 368 | 362 | 0.984 [0.983–0.984] |
| prompt | 367 | 304 | 0.829 [0.659–0.990] |
| question | 3015 | 2925 | 0.970 [0.960–0.978] |
| repeat | 485 | 458 | 0.944 [0.944–0.944] |
| session_start | 1558 | 2249 | 1.444 [1.329–1.605] |
| subagent_mcp | 269 | 274 | 1.020 [1.020–1.020] |
| subagent_start | 167 | 143 | 0.856 [0.856–0.856] |
| всего | 6228 | 6715 | 1.078 [1.035–1.134] |

- Δ (плечо − база, общие элементы, доли сессий — среднее по повторам, односторонние 95 %): hint_recall 0.000 [0.000–0.000], строго 0.000 [0.000–0.000], обязательные строки 0.000 [0.000–0.000]
- **база**: сессий 6 × 3 повторов, токенов 18685 (checkpoint 0, file_context 0, post_tool 0, pre_tool 1104, prompt 1101, question 9044, repeat 1455, session_start 4674, subagent_mcp 806, subagent_start 501); обязательные строки 69/69 (1.000); нагеты вопросов 21/21 (1.000); дайджест при двух стартах 18/18 (1.000)
  - hint_recall общий 60/60 (1.000) (строго в событии 18/60 (0.300)); подсказки-идеи — (строго —); по видам: insight 18/18 (1.000) (строго 18/18 (1.000)); rule 42/42 (1.000) (строго 0/42 (0.000)); зачёт: digest 42, event 18
  - повторная выдача 36/201 (0.179) (между каналами 105/192 (0.547)); идеи: 0; снимок сессии: 0; раздел Rules: session_start 3027; шум повторов (p95 |Δ|/среднее сессии): 0.0007
  - обязательные строки, худший повтор: r0 23/23 (1.000)
- **плечо**: сессий 6 × 3 повторов, токенов 20146 (checkpoint 0, file_context 0, post_tool 0, pre_tool 1086, prompt 913, question 8775, repeat 1374, session_start 6747, subagent_mcp 822, subagent_start 429); обязательные строки 69/69 (1.000); нагеты вопросов 21/21 (1.000); дайджест при двух стартах 18/18 (1.000)
  - hint_recall общий 60/60 (1.000) (строго в событии 18/60 (0.300)); подсказки-идеи — (строго —); по видам: insight 18/18 (1.000) (строго 18/18 (1.000)); rule 42/42 (1.000) (строго 0/42 (0.000)); зачёт: digest 42, event 18
  - повторная выдача 31/211 (0.147) (между каналами 100/205 (0.488)); идеи: session_start 1209; снимок сессии: 0; раздел Rules: session_start 3909; шум повторов (p95 |Δ|/среднее сессии): 0.0513; неразобранных выводов хука: 0, хуков сверх бюджета: 1
  - обязательные строки, худший повтор: r0 23/23 (1.000)
  - повторы с промахом бюджета хука или неразобранным выводом (действительны; блокируют приёмку 13b): ss-10 r0

### heavy (сессий 2)

| Событие | база | плечо | R |
|---|---:|---:|---|
| checkpoint | 0 | 0 | — |
| file_context | 0 | 0 | — |
| post_tool | 0 | 0 | — |
| pre_tool | 179 | 176 | 0.983 [0.982–0.984] |
| prompt | 1127 | 902 | 0.800 [0.462–1.006] |
| question | 2982 | 2622 | 0.879 [0.840–0.968] |
| repeat | 953 | 822 | 0.863 [0.863–0.863] |
| session_start | 2727 | 4397 | 1.612 [1.285–1.894] |
| subagent_mcp | 973 | 725 | 0.745 [0.745–0.745] |
| subagent_start | 658 | 538 | 0.817 [0.817–0.817] |
| всего | 9598 | 10182 | 1.061 [0.896–1.277] |

- Δ (плечо − база, общие элементы, доли сессий — среднее по повторам, односторонние 95 %): hint_recall -0.074 [-0.133–0.000], строго -0.074 [-0.133–0.000], обязательные строки 0.201 [0.000–0.302]
- **база**: сессий 2 × 3 повторов, токенов 28795 (checkpoint 0, file_context 0, post_tool 0, pre_tool 537, prompt 3381, question 8945, repeat 2858, session_start 8181, subagent_mcp 2919, subagent_start 1974); обязательные строки 115/144 (0.799); нагеты вопросов 12/12 (1.000); дайджест при двух стартах 6/6 (1.000)
  - hint_recall общий 27/27 (1.000) (строго в событии 18/27 (0.667)); подсказки-идеи — (строго —); по видам: insight 12/12 (1.000) (строго 12/12 (1.000)); rule 6/6 (1.000) (строго 0/6 (0.000)); term 9/9 (1.000) (строго 6/9 (0.667)); зачёт: digest 6, event 18, hooks 3
  - повторная выдача 66/319 (0.207) (между каналами 133/301 (0.442)); идеи: 0; снимок сессии: 0; раздел Rules: session_start 3834; шум повторов (p95 |Δ|/среднее сессии): 0.0077
  - обязательные строки, худший повтор: r1 38/48 (0.792)
  - нет строк: sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:oncall
- **плечо**: сессий 2 × 3 повторов, токенов 30546 (checkpoint 0, file_context 0, post_tool 0, pre_tool 528, prompt 2705, question 7867, repeat 2467, session_start 13191, subagent_mcp 2175, subagent_start 1613); обязательные строки 144/144 (1.000); нагеты вопросов 12/12 (1.000); дайджест при двух стартах 6/6 (1.000)
  - hint_recall общий 25/27 (0.926) (строго в событии 16/27 (0.593)); подсказки-идеи — (строго —); по видам: insight 10/12 (0.833) (строго 10/12 (0.833)); rule 6/6 (1.000) (строго 0/6 (0.000)); term 9/9 (1.000) (строго 6/9 (0.667)); зачёт: digest 6, event 16, hooks 3
  - повторная выдача 56/330 (0.170) (между каналами 117/326 (0.359)); идеи: prompt 438, session_start 1008; снимок сессии: 0; раздел Rules: session_start 7056; шум повторов (p95 |Δ|/среднее сессии): 0.0661
  - обязательные строки, худший повтор: r0 48/48 (1.000)

### Без раздела Rules (резерв `session_start`/`recovery` вычтен в обоих плечах; §17 п. 2, решение 10)

| Часть | база | плечо | R | session_start R | recovery R |
|---|---:|---:|---|---|---|
| всего (8) | 13540 | 13242 | 0.978 [0.889–1.082] | 1.497 [1.307–1.894] | — |
| standard (6) | 5219 | 5412 | 1.037 [0.984–1.104] | 1.723 [1.312–2.630] | — |
| heavy (2) | 8320 | 7830 | 0.941 [0.841–1.079] | 1.411 [1.249–1.542] | — |

- |Δ| / база токенов сессии, 95-й перцентиль: 0.2515
- бутстреп: 10000 выборок, зерно 0x5eed20261019 (парный по сессиям; токены сессии — среднее по повторам).
- повторная выдача — справочно и смещена в пользу M6: повтор `/read` с пометкой «уже выдавалось» считается помеченным, у плеча без пометки (до M6) тот же повтор — неотмеченным.
