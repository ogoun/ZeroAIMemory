# Сетка `search.budget_tokens` на dev ТС-1 к B4 — 2026-11-15 (bge-m3-fp16, CUDA)

Задача 13b, дополнение к предрегистрации (журнал этапа). Вариант Б (`--targets-of 0.0.5-m4`), K = 3: три прогона сетки `eval/grid-m6-budget-b4.toml` (три индексации) против трёх прогонов B4; статистика — `compare_reps` (среднее на сценарий, «полон» в ≥ 2 из 3, бутстреп 10 000). Сценариев 26, вне сравнения: s-20, s-46, s-47, s-48.

| `budget_tokens` | R [95 %] | токены M6 / B4 | микро-полнота B4 → M6 | Δ [нижняя] | L (все повторы) | brief: токены медиана / max, карточек медиана | критерий |
|---:|---|---|---|---|---|---|---|
| 800 | 0.906 [0.878–0.926] | 88277 / 97431 | 0.917 → 0.917 | +0.000 [+0.000] | 0 (0) | 746 / 787, 6.5 | выполнен |
| 900 | 0.931 [0.911–0.946] | 90739 / 97431 | 0.917 → 0.917 | +0.000 [+0.000] | 0 (0) | 840 / 892, 7 | выполнен |
| 1000 | 0.957 [0.943–0.967] | 93246 / 97431 | 0.917 → 0.917 | +0.000 [+0.000] | 0 (0) | 928 / 989, 8 | выполнен |
| 1100 | 0.984 [0.976–0.990] | 95880 / 97431 | 0.917 → 0.917 | +0.000 [+0.000] | 0 (0) | 1030 / 1081, 9 | выполнен |
| 1200 | 1.005 [1.002–1.010] | 97955 / 97431 | 0.917 → 0.917 | +0.000 [+0.000] | 0 (0) | 1120 / 1188, 10 | нет |

B4: brief — медиана 1076 токенов (max 1132), карточек — медиана 10 (8–10).

**Выбор по правилу:** 800 (наименьшее значение сетки, где критерий выполнен на dev).

## Файлы

- `eval/results/2026-11-15-m6-bge-m3-fp16-tokens-t13b-bud-r1-srv0.0.10-m5.7+m6-dev-b800-mean3-vs-2026-11-15-m6-bge-m3-fp16-tokens-t13b-b4dev-r1-srv0.0.5-m4-dev-mean3.json`
- `eval/results/2026-11-15-m6-bge-m3-fp16-tokens-t13b-bud-r1-srv0.0.10-m5.7+m6-dev-b900-mean3-vs-2026-11-15-m6-bge-m3-fp16-tokens-t13b-b4dev-r1-srv0.0.5-m4-dev-mean3.json`
- `eval/results/2026-11-15-m6-bge-m3-fp16-tokens-t13b-bud-r1-srv0.0.10-m5.7+m6-dev-b1000-mean3-vs-2026-11-15-m6-bge-m3-fp16-tokens-t13b-b4dev-r1-srv0.0.5-m4-dev-mean3.json`
- `eval/results/2026-11-15-m6-bge-m3-fp16-tokens-t13b-bud-r1-srv0.0.10-m5.7+m6-dev-b1100-mean3-vs-2026-11-15-m6-bge-m3-fp16-tokens-t13b-b4dev-r1-srv0.0.5-m4-dev-mean3.json`
- `eval/results/2026-11-15-m6-bge-m3-fp16-tokens-t13b-bud-r1-srv0.0.10-m5.7+m6-dev-b1200-mean3-vs-2026-11-15-m6-bge-m3-fp16-tokens-t13b-b4dev-r1-srv0.0.5-m4-dev-mean3.json`
- `.tmp/m6/task13b/ts1/budget-dev/2026-11-15-m6-bge-m3-fp16-tokens-t13b-b4dev-r1-srv0.0.5-m4-dev.json` (1846212 байт, sha256 `2cfda5e19bd82548…`)
- `.tmp/m6/task13b/ts1/budget-dev/2026-11-15-m6-bge-m3-fp16-tokens-t13b-b4dev-r2-srv0.0.5-m4-dev.json` (1846241 байт, sha256 `2e986b296880a9dc…`)
- `.tmp/m6/task13b/ts1/budget-dev/2026-11-15-m6-bge-m3-fp16-tokens-t13b-b4dev-r3-srv0.0.5-m4-dev.json` (1836296 байт, sha256 `7b85ab2b9b2bea7c…`)
- `.tmp/m6/task13b/ts1/budget-dev/2026-11-15-m6-bge-m3-fp16-tokens-t13b-bud-r1-srv0.0.10-m5.7+m6-dev-b1000.json` (2270028 байт, sha256 `68b672fb633f675c…`)
- `.tmp/m6/task13b/ts1/budget-dev/2026-11-15-m6-bge-m3-fp16-tokens-t13b-bud-r1-srv0.0.10-m5.7+m6-dev-b1100.json` (2312795 байт, sha256 `e6ab96e694eb631d…`)
- `.tmp/m6/task13b/ts1/budget-dev/2026-11-15-m6-bge-m3-fp16-tokens-t13b-bud-r1-srv0.0.10-m5.7+m6-dev-b1200.json` (2349877 байт, sha256 `b9a12720ce306c8d…`)
- `.tmp/m6/task13b/ts1/budget-dev/2026-11-15-m6-bge-m3-fp16-tokens-t13b-bud-r1-srv0.0.10-m5.7+m6-dev-b800.json` (2189176 байт, sha256 `9d9fcca1caf8be58…`)
- `.tmp/m6/task13b/ts1/budget-dev/2026-11-15-m6-bge-m3-fp16-tokens-t13b-bud-r1-srv0.0.10-m5.7+m6-dev-b900.json` (2230968 байт, sha256 `f6ab7b5739978490…`)
- `.tmp/m6/task13b/ts1/budget-dev/2026-11-15-m6-bge-m3-fp16-tokens-t13b-bud-r2-srv0.0.10-m5.7+m6-dev-b1000.json` (2264434 байт, sha256 `96b4c52e00794a24…`)
- `.tmp/m6/task13b/ts1/budget-dev/2026-11-15-m6-bge-m3-fp16-tokens-t13b-bud-r2-srv0.0.10-m5.7+m6-dev-b1100.json` (2311107 байт, sha256 `d65d778fbc7ab086…`)
- `.tmp/m6/task13b/ts1/budget-dev/2026-11-15-m6-bge-m3-fp16-tokens-t13b-bud-r2-srv0.0.10-m5.7+m6-dev-b1200.json` (2347074 байт, sha256 `f4a7e9f2ebef50be…`)
- `.tmp/m6/task13b/ts1/budget-dev/2026-11-15-m6-bge-m3-fp16-tokens-t13b-bud-r2-srv0.0.10-m5.7+m6-dev-b800.json` (2186182 байт, sha256 `dd0cddf5872d1529…`)
- `.tmp/m6/task13b/ts1/budget-dev/2026-11-15-m6-bge-m3-fp16-tokens-t13b-bud-r2-srv0.0.10-m5.7+m6-dev-b900.json` (2228812 байт, sha256 `0fbc8d2a59dc39a4…`)
- `.tmp/m6/task13b/ts1/budget-dev/2026-11-15-m6-bge-m3-fp16-tokens-t13b-bud-r3-srv0.0.10-m5.7+m6-dev-b1000.json` (2255601 байт, sha256 `564c557bde545d41…`)
- `.tmp/m6/task13b/ts1/budget-dev/2026-11-15-m6-bge-m3-fp16-tokens-t13b-bud-r3-srv0.0.10-m5.7+m6-dev-b1100.json` (2298509 байт, sha256 `35f06b6a09baa802…`)
- `.tmp/m6/task13b/ts1/budget-dev/2026-11-15-m6-bge-m3-fp16-tokens-t13b-bud-r3-srv0.0.10-m5.7+m6-dev-b1200.json` (2338796 байт, sha256 `18b65f48d676a831…`)
- `.tmp/m6/task13b/ts1/budget-dev/2026-11-15-m6-bge-m3-fp16-tokens-t13b-bud-r3-srv0.0.10-m5.7+m6-dev-b800.json` (2177331 байт, sha256 `49d9854782c3920c…`)
- `.tmp/m6/task13b/ts1/budget-dev/2026-11-15-m6-bge-m3-fp16-tokens-t13b-bud-r3-srv0.0.10-m5.7+m6-dev-b900.json` (2215487 байт, sha256 `0c956a17a6b11c8c…`)
