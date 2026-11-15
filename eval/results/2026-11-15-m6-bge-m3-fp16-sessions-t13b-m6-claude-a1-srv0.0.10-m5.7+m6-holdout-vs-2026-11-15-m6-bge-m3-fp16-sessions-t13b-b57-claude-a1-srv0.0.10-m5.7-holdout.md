
## ТС-2: 2026-11-15-m6-bge-m3-fp16-sessions-t13b-m6-claude-a1-srv0.0.10-m5.7+m6-holdout (0.0.10-m5.7+m6) против 2026-11-15-m6-bge-m3-fp16-sessions-t13b-b57-claude-a1-srv0.0.10-m5.7-holdout (0.0.10-m5.7)

- исключено из обоих плеч: события {"session_start_after_abort"}; классы строк {}; обязательные строки {}; ожидания {}

### всего (сессий 8)

| Событие | база | плечо | R |
|---|---:|---:|---|
| checkpoint | 0 | 0 | — |
| file_context | 0 | 0 | — |
| post_tool | 0 | 0 | — |
| pre_tool | 547 | 538 | 0.984 [0.983–0.984] |
| prompt | 1599 | 1206 | 0.754 [0.479–1.004] |
| question | 5998 | 5547 | 0.925 [0.878–0.975] |
| repeat | 1438 | 1280 | 0.890 [0.863–0.944] |
| session_start | 4854 | 6646 | 1.369 [1.113–1.666] |
| subagent_mcp | 1243 | 999 | 0.804 [0.744–1.020] |
| subagent_start | 825 | 681 | 0.825 [0.817–0.856] |
| всего | 16504 | 16897 | 1.024 [0.900–1.172] |

- Δ (плечо − база, общие элементы, доли сессий — среднее по повторам, односторонние 95 %): hint_recall -0.023 [-0.061–0.000], строго -0.023 [-0.061–0.000], обязательные строки 0.153 [0.000–0.266]
- **база**: сессий 8 × 3 повторов, токенов 49512 (checkpoint 0, file_context 0, post_tool 0, pre_tool 1641, prompt 4797, question 17993, repeat 4315, session_start 14563, subagent_mcp 3728, subagent_start 2475); обязательные строки 211/249 (0.847); нагеты вопросов 33/33 (1.000); дайджест при двух стартах 24/24 (1.000)
  - hint_recall общий 87/87 (1.000) (строго в событии 36/87 (0.414)); подсказки-идеи 21/21 (1.000) (строго 9/21 (0.429)); по видам: idea 21/21 (1.000) (строго 9/21 (0.429)); insight 30/30 (1.000) (строго 30/30 (1.000)); rule 48/48 (1.000) (строго 0/48 (0.000)); term 9/9 (1.000) (строго 6/9 (0.667)); зачёт: digest 60, event 45, hooks 3
  - повторная выдача 101/558 (0.181) (между каналами 233/531 (0.439)); идеи: prompt 552, session_start 2217; снимок сессии: 0; раздел Rules: session_start 6287; шум повторов (p95 |Δ|/среднее сессии): 0.0273
  - обязательные строки, худший повтор: r0 70/83 (0.843)
  - нет строк: sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:frontend-a11y; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:i18n; sh-02 session_start rule:cache-headers; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:frontend-a11y; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:i18n; sh-02 session_start rule:cache-headers; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:i18n; sh-02 session_start rule:cache-headers; sh-02 session_start rule:oncall
- **плечо**: сессий 8 × 3 повторов, токенов 50692 (checkpoint 0, file_context 0, post_tool 0, pre_tool 1614, prompt 3618, question 16642, repeat 3841, session_start 19938, subagent_mcp 2997, subagent_start 2042); обязательные строки 249/249 (1.000); нагеты вопросов 33/33 (1.000); дайджест при двух стартах 24/24 (1.000)
  - hint_recall общий 85/87 (0.977) (строго в событии 34/87 (0.391)); подсказки-идеи 19/21 (0.905) (строго 7/21 (0.333)); по видам: idea 19/21 (0.905) (строго 7/21 (0.333)); insight 28/30 (0.933) (строго 28/30 (0.933)); rule 48/48 (1.000) (строго 0/48 (0.000)); term 9/9 (1.000) (строго 6/9 (0.667)); зачёт: digest 60, event 41, hooks 3
  - повторная выдача 87/541 (0.161) (между каналами 217/531 (0.409)); идеи: prompt 438, session_start 2217; снимок сессии: 0; раздел Rules: session_start 10965; шум повторов (p95 |Δ|/среднее сессии): 0.0562; неразобранных выводов хука: 0, хуков сверх бюджета: 1
  - обязательные строки, худший повтор: r0 83/83 (1.000)
  - повторы с промахом бюджета хука или неразобранным выводом (действительны; блокируют приёмку 13b): ss-10 r0

### standard (сессий 6)

| Событие | база | плечо | R |
|---|---:|---:|---|
| checkpoint | 0 | 0 | — |
| file_context | 0 | 0 | — |
| post_tool | 0 | 0 | — |
| pre_tool | 368 | 362 | 0.984 [0.983–0.984] |
| prompt | 338 | 304 | 0.900 [0.659–1.252] |
| question | 3014 | 2925 | 0.970 [0.960–0.979] |
| repeat | 485 | 458 | 0.944 [0.944–0.944] |
| session_start | 1957 | 2249 | 1.149 [1.091–1.234] |
| subagent_mcp | 269 | 274 | 1.020 [1.020–1.020] |
| subagent_start | 167 | 143 | 0.856 [0.856–0.856] |
| всего | 6598 | 6715 | 1.018 [1.001–1.041] |

- Δ (плечо − база, общие элементы, доли сессий — среднее по повторам, односторонние 95 %): hint_recall 0.000 [0.000–0.000], строго 0.000 [0.000–0.000], обязательные строки 0.000 [0.000–0.000]
- **база**: сессий 6 × 3 повторов, токенов 19793 (checkpoint 0, file_context 0, post_tool 0, pre_tool 1104, prompt 1014, question 9042, repeat 1455, session_start 5871, subagent_mcp 806, subagent_start 501); обязательные строки 87/87 (1.000); нагеты вопросов 21/21 (1.000); дайджест при двух стартах 18/18 (1.000)
  - hint_recall общий 60/60 (1.000) (строго в событии 18/60 (0.300)); подсказки-идеи 12/12 (1.000) (строго 0/12 (0.000)); по видам: idea 12/12 (1.000) (строго 0/12 (0.000)); insight 18/18 (1.000) (строго 18/18 (1.000)); rule 42/42 (1.000) (строго 0/42 (0.000)); зачёт: digest 54, event 18
  - повторная выдача 35/218 (0.161) (между каналами 104/209 (0.498)); идеи: session_start 1209; снимок сессии: 0; раздел Rules: session_start 3027; шум повторов (p95 |Δ|/среднее сессии): 0.0314
  - обязательные строки, худший повтор: r0 29/29 (1.000)
- **плечо**: сессий 6 × 3 повторов, токенов 20146 (checkpoint 0, file_context 0, post_tool 0, pre_tool 1086, prompt 913, question 8775, repeat 1374, session_start 6747, subagent_mcp 822, subagent_start 429); обязательные строки 87/87 (1.000); нагеты вопросов 21/21 (1.000); дайджест при двух стартах 18/18 (1.000)
  - hint_recall общий 60/60 (1.000) (строго в событии 18/60 (0.300)); подсказки-идеи 12/12 (1.000) (строго 0/12 (0.000)); по видам: idea 12/12 (1.000) (строго 0/12 (0.000)); insight 18/18 (1.000) (строго 18/18 (1.000)); rule 42/42 (1.000) (строго 0/42 (0.000)); зачёт: digest 54, event 18
  - повторная выдача 31/211 (0.147) (между каналами 100/205 (0.488)); идеи: session_start 1209; снимок сессии: 0; раздел Rules: session_start 3909; шум повторов (p95 |Δ|/среднее сессии): 0.0513; неразобранных выводов хука: 0, хуков сверх бюджета: 1
  - обязательные строки, худший повтор: r0 29/29 (1.000)
  - повторы с промахом бюджета хука или неразобранным выводом (действительны; блокируют приёмку 13b): ss-10 r0

### heavy (сессий 2)

| Событие | база | плечо | R |
|---|---:|---:|---|
| checkpoint | 0 | 0 | — |
| file_context | 0 | 0 | — |
| post_tool | 0 | 0 | — |
| pre_tool | 179 | 176 | 0.983 [0.982–0.984] |
| prompt | 1261 | 902 | 0.715 [0.407–0.906] |
| question | 2984 | 2622 | 0.879 [0.840–0.966] |
| repeat | 953 | 822 | 0.863 [0.863–0.863] |
| session_start | 2897 | 4397 | 1.518 [1.139–1.883] |
| subagent_mcp | 974 | 725 | 0.744 [0.744–0.744] |
| subagent_start | 658 | 538 | 0.817 [0.817–0.817] |
| всего | 9906 | 10182 | 1.028 [0.861–1.251] |

- Δ (плечо − база, общие элементы, доли сессий — среднее по повторам, односторонние 95 %): hint_recall -0.074 [-0.133–0.000], строго -0.074 [-0.133–0.000], обязательные строки 0.235 [0.000–0.362]
- **база**: сессий 2 × 3 повторов, токенов 29719 (checkpoint 0, file_context 0, post_tool 0, pre_tool 537, prompt 3783, question 8951, repeat 2860, session_start 8692, subagent_mcp 2922, subagent_start 1974); обязательные строки 124/162 (0.765); нагеты вопросов 12/12 (1.000); дайджест при двух стартах 6/6 (1.000)
  - hint_recall общий 27/27 (1.000) (строго в событии 18/27 (0.667)); подсказки-идеи 9/9 (1.000) (строго 9/9 (1.000)); по видам: idea 9/9 (1.000) (строго 9/9 (1.000)); insight 12/12 (1.000) (строго 12/12 (1.000)); rule 6/6 (1.000) (строго 0/6 (0.000)); term 9/9 (1.000) (строго 6/9 (0.667)); зачёт: digest 6, event 27, hooks 3
  - повторная выдача 66/340 (0.194) (между каналами 129/322 (0.401)); идеи: prompt 552, session_start 1008; снимок сессии: 0; раздел Rules: session_start 3260; шум повторов (p95 |Δ|/среднее сессии): 0.0060
  - обязательные строки, худший повтор: r0 41/54 (0.759)
  - нет строк: sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:frontend-a11y; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:i18n; sh-02 session_start rule:cache-headers; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:frontend-a11y; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:i18n; sh-02 session_start rule:cache-headers; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:i18n; sh-02 session_start rule:cache-headers; sh-02 session_start rule:oncall
- **плечо**: сессий 2 × 3 повторов, токенов 30546 (checkpoint 0, file_context 0, post_tool 0, pre_tool 528, prompt 2705, question 7867, repeat 2467, session_start 13191, subagent_mcp 2175, subagent_start 1613); обязательные строки 162/162 (1.000); нагеты вопросов 12/12 (1.000); дайджест при двух стартах 6/6 (1.000)
  - hint_recall общий 25/27 (0.926) (строго в событии 16/27 (0.593)); подсказки-идеи 7/9 (0.778) (строго 7/9 (0.778)); по видам: idea 7/9 (0.778) (строго 7/9 (0.778)); insight 10/12 (0.833) (строго 10/12 (0.833)); rule 6/6 (1.000) (строго 0/6 (0.000)); term 9/9 (1.000) (строго 6/9 (0.667)); зачёт: digest 6, event 23, hooks 3
  - повторная выдача 56/330 (0.170) (между каналами 117/326 (0.359)); идеи: prompt 438, session_start 1008; снимок сессии: 0; раздел Rules: session_start 7056; шум повторов (p95 |Δ|/среднее сессии): 0.0661
  - обязательные строки, худший повтор: r0 54/54 (1.000)

### Без раздела Rules (резерв `session_start`/`recovery` вычтен в обоих плечах; §17 п. 2, решение 10)

| Часть | база | плечо | R | session_start R | recovery R |
|---|---:|---:|---|---|---|
| всего (8) | 14408 | 13242 | 0.919 [0.845–0.992] | 1.084 [0.997–1.181] | — |
| standard (6) | 5589 | 5412 | 0.968 [0.953–0.988] | 0.998 [0.997–1.000] | — |
| heavy (2) | 8820 | 7830 | 0.888 [0.804–1.000] | 1.129 [0.999–1.235] | — |

- подсказки-идеи prompt совпали с базой: 24/24 (1.000)
- |Δ| / база токенов сессии, 95-й перцентиль: 0.2118
- бутстреп: 10000 выборок, зерно 0x5eed20261019 (парный по сессиям; токены сессии — среднее по повторам).
- повторная выдача — справочно и смещена в пользу M6: повтор `/read` с пометкой «уже выдавалось» считается помеченным, у плеча без пометки (до M6) тот же повтор — неотмеченным.
