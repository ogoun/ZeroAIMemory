# ТС-2 «сессия» — 2026-11-15 (holdout, сервер 0.0.10-m5.7+m6, bge-m3-fp16, идеи on, MCP explicit)

- возможности: Feats { journal: true, glossary: true, subagent: true, recovery: true, file_context: true, code_map: true, ideas: true, ideas_on: true, snapshot: true, mcp_explicit: true }
- не воспроизводятся у сервера: {}; классы строк, которых нет: {}; виды обязательных строк, которых нет: {}
- агент хуков — claude (`zam hook --agent`, вывод с пределом канала)
- MCP — zam mcp с ZAM_SESSION_ID (явная сессия)
- **итог**: сессий 8 × 3 повторов, токенов 59518 (checkpoint 0, file_context 0, post_tool 0, pre_tool 1614, prompt 3618, question 16642, repeat 3841, session_start 19938, session_start_after_abort 8826, subagent_mcp 2997, subagent_start 2042); обязательные строки 252/252 (1.000); нагеты вопросов 33/33 (1.000); дайджест при двух стартах 24/24 (1.000)
  - hint_recall общий 85/87 (0.977) (строго в событии 34/87 (0.391)); подсказки-идеи 19/21 (0.905) (строго 7/21 (0.333)); по видам: idea 19/21 (0.905) (строго 7/21 (0.333)); insight 28/30 (0.933) (строго 28/30 (0.933)); rule 48/48 (1.000) (строго 0/48 (0.000)); term 9/9 (1.000) (строго 6/9 (0.667)); зачёт: digest 60, event 41, hooks 3
  - повторная выдача 87/541 (0.161) (между каналами 217/531 (0.409)); идеи: prompt 438, session_start 2217, session_start_after_abort 519; снимок сессии: session_start_after_abort 501; раздел Rules: session_start 10965; шум повторов (p95 |Δ|/среднее сессии): 0.0562; неразобранных выводов хука: 0, хуков сверх бюджета: 1
  - обязательные строки, худший повтор: r0 84/84 (1.000)
  - повторы с промахом бюджета хука или неразобранным выводом (действительны; блокируют приёмку 13b): ss-10 r0

| Сессия | профиль | токенов | обязательные | hint общий | идеи | нагеты | дайджест = |
|---|---|---:|---|---|---|---|---|
| ss-02 | standard | 1103 | 4/4 (1.000) | 4/4 (1.000) | 1/1 (1.000) | 1/1 (1.000) | да |
| ss-08 | standard | 1097 | 6/6 (1.000) | 2/2 (1.000) | 1/1 (1.000) | 1/1 (1.000) | да |
| sh-02 | heavy | 8198 | 36/36 (1.000) | 4/4 (1.000) | 2/2 (1.000) | 2/2 (1.000) | да |
| sh-03 | heavy | 5224 | 19/19 (1.000) | 5/5 (1.000) | 1/1 (1.000) | 2/2 (1.000) | да |
| ss-10 | standard | 735 | 4/4 (1.000) | 2/2 (1.000) | 1/1 (1.000) | 1/1 (1.000) | да |
| ss-11 | standard | 1562 | 4/4 (1.000) | 3/3 (1.000) | — | 2/2 (1.000) | да |
| ss-14 | standard | 1375 | 7/7 (1.000) | 4/4 (1.000) | 1/1 (1.000) | 1/1 (1.000) | да |
| ss-15 | standard | 904 | 4/4 (1.000) | 5/5 (1.000) | — | 1/1 (1.000) | да |
| ss-02 | standard | 1099 | 4/4 (1.000) | 4/4 (1.000) | 1/1 (1.000) | 1/1 (1.000) | да |
| ss-08 | standard | 1097 | 6/6 (1.000) | 2/2 (1.000) | 1/1 (1.000) | 1/1 (1.000) | да |
| sh-02 | heavy | 8280 | 36/36 (1.000) | 4/4 (1.000) | 2/2 (1.000) | 2/2 (1.000) | да |
| sh-03 | heavy | 4641 | 19/19 (1.000) | 4/5 (0.800) | 0/1 (0.000) | 2/2 (1.000) | да |
| ss-10 | standard | 736 | 4/4 (1.000) | 2/2 (1.000) | 1/1 (1.000) | 1/1 (1.000) | да |
| ss-11 | standard | 1563 | 4/4 (1.000) | 3/3 (1.000) | — | 2/2 (1.000) | да |
| ss-14 | standard | 1375 | 7/7 (1.000) | 4/4 (1.000) | 1/1 (1.000) | 1/1 (1.000) | да |
| ss-15 | standard | 903 | 4/4 (1.000) | 5/5 (1.000) | — | 1/1 (1.000) | да |
| ss-02 | standard | 1099 | 4/4 (1.000) | 4/4 (1.000) | 1/1 (1.000) | 1/1 (1.000) | да |
| ss-08 | standard | 1098 | 6/6 (1.000) | 2/2 (1.000) | 1/1 (1.000) | 1/1 (1.000) | да |
| sh-02 | heavy | 8280 | 36/36 (1.000) | 4/4 (1.000) | 2/2 (1.000) | 2/2 (1.000) | да |
| sh-03 | heavy | 4749 | 19/19 (1.000) | 4/5 (0.800) | 0/1 (0.000) | 2/2 (1.000) | да |
| ss-10 | standard | 736 | 4/4 (1.000) | 2/2 (1.000) | 1/1 (1.000) | 1/1 (1.000) | да |
| ss-11 | standard | 1562 | 4/4 (1.000) | 3/3 (1.000) | — | 2/2 (1.000) | да |
| ss-14 | standard | 1274 | 7/7 (1.000) | 4/4 (1.000) | 1/1 (1.000) | 1/1 (1.000) | да |
| ss-15 | standard | 828 | 4/4 (1.000) | 5/5 (1.000) | — | 1/1 (1.000) | да |

Примечания:
- ss-02: hooks.log (prompt): 2026-11-15T03:16:48Z code-sync карта кода: файлов 0, к загрузке 0, загружено 0, удалено 0 (184 мс)
- ss-08: hooks.log (checkpoint): 2026-11-15T03:17:12Z code-sync карта кода: файлов 0, к загрузке 0, загружено 0, удалено 0 (179 мс)
- sh-02: zam code sync: карта кода: файлов 3, к загрузке 3, загружено 3, удалено 0 (344 мс)
- sh-02: hooks.log (checkpoint): 2026-11-15T03:17:40Z checkpoint снимок stop: 585 байт, complete: ответ получен (HTTP 204), очередь не обработана до выхода хука
- sh-03: zam code sync: карта кода: файлов 3, к загрузке 3, загружено 3, удалено 0 (329 мс)
- ss-10: hooks.log (prompt): 2026-11-15T03:18:33Z prompt recall(prompt): предупреждение сервера: recall timeout
- ss-10: hooks.log (prompt): 2026-11-15T03:18:34Z prompt recall(prompt): предупреждение сервера: recall timeout
- ss-10: hooks.log (prompt): 2026-11-15T03:18:34Z prompt recall(prompt): сервер http://127.0.0.1:49603 недоступен: timeout: global
- ss-11: hooks.log (prompt): 2026-11-15T03:19:03Z prompt recall(prompt): предупреждение сервера: recall timeout
- ss-11: hooks.log (prompt): 2026-11-15T03:19:03Z prompt recall(prompt): предупреждение сервера: recall timeout
- ss-11: hooks.log (subagent-start): 2026-11-15T03:19:04Z subagent-start recall(brief): предупреждение сервера: recall timeout
- ss-14: hooks.log (prompt): 2026-11-15T03:19:32Z code-sync карта кода: файлов 0, к загрузке 0, загружено 0, удалено 0 (184 мс)
- ss-02: hooks.log (prompt): 2026-11-15T03:20:17Z code-sync карта кода: файлов 0, к загрузке 0, загружено 0, удалено 0 (181 мс)
- ss-08: hooks.log (prompt): 2026-11-15T03:20:40Z code-sync карта кода: файлов 0, к загрузке 0, загружено 0, удалено 0 (193 мс)
- sh-02: zam code sync: карта кода: файлов 3, к загрузке 3, загружено 3, удалено 0 (218 мс)
- sh-02: hooks.log (checkpoint): 2026-11-15T03:21:12Z checkpoint снимок stop: 585 байт, complete, отправлен
- sh-03: zam code sync: карта кода: файлов 3, к загрузке 3, загружено 3, удалено 0 (261 мс)
- sh-03: hooks.log (prompt): 2026-11-15T03:21:46Z prompt recall(prompt): предупреждение сервера: recall timeout
- sh-03: hooks.log (prompt): 2026-11-15T03:21:46Z prompt recall(prompt): предупреждение сервера: recall timeout
- sh-03: hooks.log (subagent-start): 2026-11-15T03:21:47Z subagent-start recall(brief): предупреждение сервера: recall timeout
- sh-03: hooks.log (prompt): 2026-11-15T03:21:47Z prompt recall(prompt): предупреждение сервера: recall timeout
- ss-10: hooks.log (prompt): 2026-11-15T03:22:18Z prompt recall(prompt): предупреждение сервера: recall timeout
- ss-10: hooks.log (prompt): 2026-11-15T03:22:18Z code-sync карта кода: файлов 0, к загрузке 0, загружено 0, удалено 0 (246 мс)
- ss-10: hooks.log (prompt): 2026-11-15T03:22:19Z prompt recall(prompt): предупреждение сервера: recall timeout
- ss-10: hooks.log (prompt): 2026-11-15T03:22:19Z prompt recall(prompt): предупреждение сервера: recall timeout
- ss-11: hooks.log (prompt): 2026-11-15T03:22:49Z code-sync карта кода: файлов 0, к загрузке 0, загружено 0, удалено 0 (225 мс)
- ss-14: hooks.log (prompt): 2026-11-15T03:23:15Z code-sync карта кода: файлов 0, к загрузке 0, загружено 0, удалено 0 (213 мс)
- ss-15: hooks.log (prompt): 2026-11-15T03:23:40Z code-sync карта кода: файлов 0, к загрузке 0, загружено 0, удалено 0 (230 мс)
- ss-02: hooks.log (post-tool): 2026-11-15T03:24:06Z code-sync карта кода: файлов 0, к загрузке 0, загружено 0, удалено 0 (213 мс)
- ss-08: hooks.log (prompt): 2026-11-15T03:24:30Z code-sync карта кода: файлов 0, к загрузке 0, загружено 0, удалено 0 (304 мс)
- sh-02: zam code sync: карта кода: файлов 3, к загрузке 3, загружено 3, удалено 0 (239 мс)
- sh-02: hooks.log (checkpoint): 2026-11-15T03:25:02Z checkpoint снимок stop: 585 байт, complete, отправлен
- sh-03: zam code sync: карта кода: файлов 3, к загрузке 3, загружено 3, удалено 0 (289 мс)
- sh-03: hooks.log (prompt): 2026-11-15T03:25:44Z prompt recall(prompt): предупреждение сервера: recall timeout
- sh-03: hooks.log (prompt): 2026-11-15T03:25:46Z prompt recall(prompt): предупреждение сервера: recall timeout
- sh-03: hooks.log (subagent-start): 2026-11-15T03:25:46Z subagent-start recall(brief): предупреждение сервера: recall timeout
- sh-03: hooks.log (prompt): 2026-11-15T03:25:47Z prompt recall(prompt): предупреждение сервера: recall timeout
- ss-10: hooks.log (prompt): 2026-11-15T03:26:24Z code-sync карта кода: файлов 0, к загрузке 0, загружено 0, удалено 0 (218 мс)
- ss-10: hooks.log (prompt): 2026-11-15T03:26:25Z prompt recall(prompt): предупреждение сервера: recall timeout
- ss-10: hooks.log (prompt): 2026-11-15T03:26:26Z prompt recall(prompt): предупреждение сервера: recall timeout
- ss-10: hooks.log (prompt): 2026-11-15T03:26:26Z prompt recall(prompt): предупреждение сервера: recall timeout
- ss-11: hooks.log (prompt): 2026-11-15T03:27:03Z code-sync карта кода: файлов 0, к загрузке 0, загружено 0, удалено 0 (232 мс)
- ss-11: hooks.log (prompt): 2026-11-15T03:27:03Z prompt recall(prompt): предупреждение сервера: recall timeout
- ss-11: hooks.log (prompt): 2026-11-15T03:27:04Z prompt recall(prompt): предупреждение сервера: recall timeout
- ss-11: hooks.log (prompt): 2026-11-15T03:27:05Z prompt recall(prompt): предупреждение сервера: recall timeout
- ss-11: hooks.log (subagent-start): 2026-11-15T03:27:05Z subagent-start recall(brief): предупреждение сервера: recall timeout
- ss-14: hooks.log (prompt): 2026-11-15T03:27:51Z prompt recall(prompt): предупреждение сервера: recall timeout
- ss-14: hooks.log (prompt): 2026-11-15T03:27:53Z prompt recall(prompt): предупреждение сервера: recall timeout
- ss-14: hooks.log (prompt): 2026-11-15T03:27:53Z prompt recall(prompt): предупреждение сервера: recall timeout
- ss-15: hooks.log (prompt): 2026-11-15T03:28:33Z prompt recall(prompt): предупреждение сервера: recall timeout
- ss-15: hooks.log (prompt): 2026-11-15T03:28:34Z prompt recall(prompt): предупреждение сервера: recall timeout
- ss-15: hooks.log (prompt): 2026-11-15T03:28:34Z prompt recall(prompt): предупреждение сервера: recall timeout
