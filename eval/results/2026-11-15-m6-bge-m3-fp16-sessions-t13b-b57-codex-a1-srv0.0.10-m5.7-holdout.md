# ТС-2 «сессия» — 2026-11-15 (holdout, сервер 0.0.10-m5.7, bge-m3-fp16, идеи on, MCP implicit)

- возможности: Feats { journal: true, glossary: true, subagent: true, recovery: true, file_context: true, code_map: true, ideas: true, ideas_on: true, snapshot: false, mcp_explicit: false }
- не воспроизводятся у сервера: {"session_start_after_abort"}; классы строк, которых нет: {}; виды обязательных строк, которых нет: {}
- агент хуков — codex (`zam hook --agent`, вывод с пределом канала)
- MCP — zam mcp без ZAM_SESSION_ID и без файла привязки (неявный путь R13)
- **итог**: сессий 8 × 3 повторов, токенов 49616 (checkpoint 0, file_context 0, post_tool 0, pre_tool 1641, prompt 4929, question 17990, repeat 4315, session_start 14539, subagent_mcp 3727, subagent_start 2475); обязательные строки 211/249 (0.847); нагеты вопросов 33/33 (1.000); дайджест при двух стартах 24/24 (1.000)
  - hint_recall общий 87/87 (1.000) (строго в событии 36/87 (0.414)); подсказки-идеи 21/21 (1.000) (строго 9/21 (0.429)); по видам: idea 21/21 (1.000) (строго 9/21 (0.429)); insight 30/30 (1.000) (строго 30/30 (1.000)); rule 48/48 (1.000) (строго 0/48 (0.000)); term 9/9 (1.000) (строго 6/9 (0.667)); зачёт: digest 60, event 45, hooks 3
  - повторная выдача 102/558 (0.183) (между каналами 234/531 (0.441)); идеи: prompt 552, session_start 2217; снимок сессии: 0; раздел Rules: session_start 6279; шум повторов (p95 |Δ|/среднее сессии): 0.0083
  - обязательные строки, худший повтор: r0 70/83 (0.843)
  - нет строк: sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:frontend-a11y; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:i18n; sh-02 session_start rule:cache-headers; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:frontend-a11y; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:i18n; sh-02 session_start rule:cache-headers; sh-02 session_start rule:oncall; sh-02 session_start rule:commits; sh-02 session_start rule:secrets; sh-02 session_start rule:tests; sh-02 session_start rule:deps; sh-02 session_start rule:frontend-a11y; sh-02 session_start rule:release; sh-02 session_start rule:infra-changes; sh-02 session_start rule:tmp-files; sh-02 session_start rule:reviews; sh-02 session_start rule:feature-flags; sh-02 session_start rule:i18n; sh-02 session_start rule:cache-headers; sh-02 session_start rule:oncall

| Сессия | профиль | токенов | обязательные | hint общий | идеи | нагеты | дайджест = |
|---|---|---:|---|---|---|---|---|
| ss-02 | standard | 1093 | 4/4 (1.000) | 4/4 (1.000) | 1/1 (1.000) | 1/1 (1.000) | да |
| ss-08 | standard | 1075 | 6/6 (1.000) | 2/2 (1.000) | 1/1 (1.000) | 1/1 (1.000) | да |
| sh-02 | heavy | 4232 | 22/35 (0.629) | 4/4 (1.000) | 2/2 (1.000) | 2/2 (1.000) | да |
| sh-03 | heavy | 5659 | 19/19 (1.000) | 5/5 (1.000) | 1/1 (1.000) | 2/2 (1.000) | да |
| ss-10 | standard | 689 | 4/4 (1.000) | 2/2 (1.000) | 1/1 (1.000) | 1/1 (1.000) | да |
| ss-11 | standard | 1560 | 4/4 (1.000) | 3/3 (1.000) | — | 2/2 (1.000) | да |
| ss-14 | standard | 1363 | 7/7 (1.000) | 4/4 (1.000) | 1/1 (1.000) | 1/1 (1.000) | да |
| ss-15 | standard | 847 | 4/4 (1.000) | 5/5 (1.000) | — | 1/1 (1.000) | да |
| ss-02 | standard | 1092 | 4/4 (1.000) | 4/4 (1.000) | 1/1 (1.000) | 1/1 (1.000) | да |
| ss-08 | standard | 1074 | 6/6 (1.000) | 2/2 (1.000) | 1/1 (1.000) | 1/1 (1.000) | да |
| sh-02 | heavy | 4311 | 23/35 (0.657) | 4/4 (1.000) | 2/2 (1.000) | 2/2 (1.000) | да |
| sh-03 | heavy | 5660 | 19/19 (1.000) | 5/5 (1.000) | 1/1 (1.000) | 2/2 (1.000) | да |
| ss-10 | standard | 689 | 4/4 (1.000) | 2/2 (1.000) | 1/1 (1.000) | 1/1 (1.000) | да |
| ss-11 | standard | 1561 | 4/4 (1.000) | 3/3 (1.000) | — | 2/2 (1.000) | да |
| ss-14 | standard | 1363 | 7/7 (1.000) | 4/4 (1.000) | 1/1 (1.000) | 1/1 (1.000) | да |
| ss-15 | standard | 848 | 4/4 (1.000) | 5/5 (1.000) | — | 1/1 (1.000) | да |
| ss-02 | standard | 1092 | 4/4 (1.000) | 4/4 (1.000) | 1/1 (1.000) | 1/1 (1.000) | да |
| ss-08 | standard | 1074 | 6/6 (1.000) | 2/2 (1.000) | 1/1 (1.000) | 1/1 (1.000) | да |
| sh-02 | heavy | 4215 | 22/35 (0.629) | 4/4 (1.000) | 2/2 (1.000) | 2/2 (1.000) | да |
| sh-03 | heavy | 5660 | 19/19 (1.000) | 5/5 (1.000) | 1/1 (1.000) | 2/2 (1.000) | да |
| ss-10 | standard | 689 | 4/4 (1.000) | 2/2 (1.000) | 1/1 (1.000) | 1/1 (1.000) | да |
| ss-11 | standard | 1560 | 4/4 (1.000) | 3/3 (1.000) | — | 2/2 (1.000) | да |
| ss-14 | standard | 1363 | 7/7 (1.000) | 4/4 (1.000) | 1/1 (1.000) | 1/1 (1.000) | да |
| ss-15 | standard | 847 | 4/4 (1.000) | 5/5 (1.000) | — | 1/1 (1.000) | да |

Примечания:
- sh-02: zam code sync: карта кода: файлов 3, к загрузке 3, загружено 3, удалено 0 (347 мс)
- sh-03: zam code sync: карта кода: файлов 3, к загрузке 3, загружено 3, удалено 0 (200 мс)
- ss-10: hooks.log (prompt): 2026-11-15T04:13:22Z code-sync карта кода: файлов 0, к загрузке 0, загружено 0, удалено 0 (182 мс)
- ss-11: hooks.log (prompt): 2026-11-15T04:13:45Z code-sync карта кода: файлов 0, к загрузке 0, загружено 0, удалено 0 (187 мс)
- ss-14: hooks.log (prompt): 2026-11-15T04:14:07Z code-sync карта кода: файлов 0, к загрузке 0, загружено 0, удалено 0 (187 мс)
- ss-15: hooks.log (checkpoint): 2026-11-15T04:14:30Z code-sync карта кода: файлов 0, к загрузке 0, загружено 0, удалено 0 (171 мс)
- ss-02: hooks.log (prompt): 2026-11-15T04:14:53Z code-sync карта кода: файлов 0, к загрузке 0, загружено 0, удалено 0 (185 мс)
- ss-08: hooks.log (checkpoint): 2026-11-15T04:15:15Z code-sync карта кода: файлов 0, к загрузке 0, загружено 0, удалено 0 (180 мс)
- sh-02: zam code sync: карта кода: файлов 3, к загрузке 3, загружено 3, удалено 0 (259 мс)
- sh-03: zam code sync: карта кода: файлов 3, к загрузке 3, загружено 3, удалено 0 (209 мс)
- ss-11: hooks.log (subagent-start): 2026-11-15T04:16:53Z code-sync карта кода: файлов 0, к загрузке 0, загружено 0, удалено 0 (186 мс)
- ss-14: hooks.log (checkpoint): 2026-11-15T04:17:16Z code-sync карта кода: файлов 0, к загрузке 0, загружено 0, удалено 0 (211 мс)
- ss-02: hooks.log (prompt): 2026-11-15T04:18:03Z code-sync карта кода: файлов 0, к загрузке 0, загружено 0, удалено 0 (187 мс)
- ss-08: hooks.log (prompt): 2026-11-15T04:18:25Z code-sync карта кода: файлов 0, к загрузке 0, загружено 0, удалено 0 (185 мс)
- sh-02: zam code sync: карта кода: файлов 3, к загрузке 3, загружено 3, удалено 0 (210 мс)
- sh-03: zam code sync: карта кода: файлов 3, к загрузке 3, загружено 3, удалено 0 (195 мс)
- ss-11: hooks.log (subagent-start): 2026-11-15T04:20:04Z code-sync карта кода: файлов 0, к загрузке 0, загружено 0, удалено 0 (189 мс)
- ss-14: hooks.log (prompt): 2026-11-15T04:20:28Z code-sync карта кода: файлов 0, к загрузке 0, загружено 0, удалено 0 (170 мс)
