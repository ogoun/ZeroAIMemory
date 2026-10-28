# Подсказки хуков: bge-m3-int8 cpu (2026-10-28)

Коммит `6943aea`, модель `bge-m3-int8@a206e10e`, отпечаток корпуса `3e0777bde1b82224`, набора подсказок `fd6759e0d4d2d3f2`, индексация 396 с; переопределения: {}.

## Точность

| | случаев (рел./нерел.) | hint_precision | hint_recall | silence_rate | случаев с попаданием | ожидаемых/показано |
|---|---|---|---|---|---|---|
| все | 72 (36/36) | 0.737 [0.593…0.872] | 0.808 [0.667…0.930] | 0.806 [0.667…0.917] | 0.917 [0.806…1.000] | 42/57 |
| post_tool | 24 (12/12) | 0.522 [0.280…0.800] | 0.706 [0.500…0.929] | 0.583 [0.333…0.833] | 0.917 [0.750…1.000] | 12/23 |
| pre_tool | 18 (9/9) | 1.000 [1.000…1.000] | 1.000 [1.000…1.000] | 1.000 [1.000…1.000] | 1.000 [1.000…1.000] | 9/9 |
| prompt | 30 (15/15) | 0.840 [0.650…1.000] | 0.808 [0.571…1.000] | 0.867 [0.667…1.000] | 0.867 [0.667…1.000] | 21/25 |

## Калибровка

`recall.prompt_min_score` (модель отбора совпала с `/recall` при 0,60 в 97 % случаев):

| порог | hint_precision | hint_recall | silence_rate | ожидаемых/показано | Δ precision | Δ recall | Δ silence |
|---|---|---|---|---|---|---|---|
| 0.30 | 0.436 [0.286…0.600] | 0.923 [0.833…1.000] | 0.267 [0.067…0.533] | 24/55 | -0.371 [-0.586…-0.199] | +0.115 [+0.000…+0.286] | -0.600 [-0.800…-0.333] |
| 0.35 | 0.453 [0.298…0.621] | 0.923 [0.833…1.000] | 0.333 [0.133…0.600] | 24/53 | -0.355 [-0.576…-0.180] | +0.115 [+0.000…+0.286] | -0.533 [-0.800…-0.267] |
| 0.40 | 0.471 [0.310…0.642] | 0.923 [0.833…1.000] | 0.400 [0.133…0.667] | 24/51 | -0.337 [-0.551…-0.164] | +0.115 [+0.000…+0.286] | -0.467 [-0.667…-0.200] |
| 0.45 | 0.571 [0.395…0.750] | 0.923 [0.833…1.000] | 0.467 [0.200…0.733] | 24/42 | -0.236 [-0.437…-0.088] | +0.115 [+0.000…+0.286] | -0.400 [-0.667…-0.200] |
| 0.50 | 0.629 [0.433…0.811] | 0.846 [0.680…1.000] | 0.533 [0.267…0.800] | 22/35 | -0.179 [-0.356…-0.057] | +0.038 [+0.000…+0.120] | -0.333 [-0.600…-0.133] |
| 0.55 | 0.750 [0.516…0.960] | 0.808 [0.571…1.000] | 0.800 [0.600…1.000] | 21/28 | -0.058 [-0.206…+0.000] | 0 | -0.067 [-0.200…+0.000] |
| **0.60** (сейчас) | 0.808 [0.579…1.000] | 0.808 [0.571…1.000] | 0.867 [0.667…1.000] | 21/26 | 0 | 0 | 0 |
| 0.65 | 0.812 [0.500…1.000] | 0.500 [0.250…0.714] | 0.867 [0.667…1.000] | 13/16 | +0.005 [-0.140…+0.101] | -0.308 [-0.524…-0.129] | 0 |
| 0.70 | 0.778 [0.286…1.000] | 0.269 [0.040…0.484] | 0.867 [0.667…1.000] | 7/9 | -0.030 [-0.393…+0.096] | -0.538 [-0.773…-0.310] | 0 |
| 0.75 | 0.833 [0.000…1.000] | 0.192 [0.000…0.417] | 0.933 [0.800…1.000] | 5/6 | +0.026 [-0.478…+0.174] | -0.615 [-0.846…-0.364] | +0.067 [+0.000…+0.200] |
| 0.80 | 0.800 [0.000…1.000] | 0.154 [0.000…0.391] | 0.933 [0.800…1.000] | 4/5 | -0.008 [-0.792…+0.172] | -0.654 [-0.900…-0.387] | +0.067 [+0.000…+0.200] |
| 0.85 | 0.750 [0.000…1.000] | 0.115 [0.000…0.286] | 0.933 [0.800…1.000] | 3/4 | -0.058 [-0.792…+0.167] | -0.692 [-0.905…-0.458] | +0.067 [+0.000…+0.200] |
| 0.90 | 0.750 [0.000…1.000] | 0.115 [0.000…0.286] | 0.933 [0.800…1.000] | 3/4 | -0.058 [-0.792…+0.167] | -0.692 [-0.905…-0.458] | +0.067 [+0.000…+0.200] |

Порог покрытия ослабленного FTS `post_tool` (модель совпала с `/recall` при текущем правиле в 100 % случаев):

| правило | hint_precision | hint_recall | silence_rate | ожидаемых/показано | Δ precision | Δ recall | Δ silence |
|---|---|---|---|---|---|---|---|
| **clamp(⌈n/3⌉, 2, 4)** (сейчас) | 0.522 [0.280…0.800] | 0.706 [0.500…0.929] | 0.583 [0.333…0.833] | 12/23 | 0 | 0 | 0 |
| ровно 1 | 0.206 [0.106…0.317] | 0.765 [0.550…1.000] | 0.000 [0.000…0.000] | 13/63 | -0.315 [-0.537…-0.151] | +0.059 [+0.000…+0.176] | -0.583 [-0.833…-0.333] |
| ровно 2 | 0.310 [0.160…0.500] | 0.765 [0.550…1.000] | 0.333 [0.083…0.583] | 13/42 | -0.212 [-0.378…-0.090] | +0.059 [+0.000…+0.176] | -0.250 [-0.500…+0.000] |
| ровно 3 | 0.522 [0.281…0.810] | 0.706 [0.500…0.929] | 0.583 [0.333…0.833] | 12/23 | +0.000 [-0.088…+0.144] | 0 | 0 |
| clamp(⌈n/2⌉, 2, 4) | 0.667 [0.409…0.929] | 0.706 [0.500…0.929] | 0.833 [0.583…1.000] | 12/18 | +0.145 [+0.000…+0.356] | 0 | +0.250 [+0.000…+0.500] |
| clamp(⌈n/3⌉, 2, 6) | 0.545 [0.304…0.815] | 0.706 [0.500…0.929] | 0.583 [0.333…0.833] | 12/22 | +0.024 [+0.000…+0.079] | 0 | 0 |
| clamp(⌈n/4⌉, 2, 4) | 0.400 [0.219…0.652] | 0.706 [0.500…0.929] | 0.500 [0.250…0.750] | 12/30 | -0.122 [-0.243…-0.036] | 0 | -0.083 [-0.250…+0.000] |
| только строгий запрос | 1.000 [1.000…1.000] | 0.412 [0.200…0.667] | 1.000 [1.000…1.000] | 7/7 | +0.478 [+0.200…+0.720] | -0.294 [-0.471…-0.118] | +0.417 [+0.167…+0.667] |

## Задержки, мс

| | n | p50 | p95 | max | сервер p50 / p95 | бюджет | дольше бюджета | `recall timeout` | непустых |
|---|---|---|---|---|---|---|---|---|---|
| `/recall` post_tool | 120 | 7.8 | 11.0 | 12.9 | 5.7 / 8.9 | 250 | 0.0 % | 0 | 85 |
| `/recall` prompt | 150 | 64.1 | 89.2 | 199.4 | 61.8 / 87.1 | 250 | 0.0 % | 0 | 35 |
| `/recall` session_start | 40 | 14.1 | 16.2 | 20.0 | 12.0 / 14.1 | 250 | 0.0 % | 0 | 40 |
| `zam hook post-tool` | 72 | 23.4 | 30.2 | 45.9 | — | 250 | 0.0 % | 0 | 51 |
| `zam hook pre-tool` | 54 | 11.7 | 16.5 | 53.6 | — | 50 | 1.9 % | 0 | 27 |
| `zam hook prompt` | 100 | 81.4 | 100.8 | 199.3 | — | 250 | 0.0 % | 0 | 61 |
| `zam hook session-start` | 40 | 33.2 | 37.2 | 41.7 | — | 300 | 0.0 % | 0 | 40 |

Строк в `hooks.log` проекта: 0.

## Случаи

| id | событие | ожидалось | показано | мс |
|---|---|---|---|---|
| p-01 | prompt | rule:commits | rule:commits | 53 |
| p-02 | prompt | rule:deps | — | 59 |
| p-03 | prompt | rule:tmp-files, insight:p13-a, insight:p13-h | — | 65 |
| p-04 | prompt | insight:p09-t, insight:p09-h | insight:p09-t, insight:p09-h | 78 |
| p-05 | prompt | insight:p01-t, insight:p01-h, insight:h02-ps1-bom | insight:p01-t, insight:p01-h, insight:h02-ps1-bom | 72 |
| p-06 | prompt | insight:p02-t, insight:p02-h, insight:p12-a, insight:p12-h, insight:h07-no-docker | insight:p02-t, insight:p12-a, insight:h07-no-docker | 60 |
| p-07 | prompt | insight:p11-a, insight:p11-h, insight:h05-force-push | insight:p11-a, insight:h05-force-push, insight:p11-h | 47 |
| p-08 | prompt | insight:p03-t, insight:p03-h, insight:h13-cuda-dll | insight:p03-t, insight:p03-h, insight:p12-a | 73 |
| p-09 | prompt | insight:c07-b | insight:c07-b | 55 |
| p-10 | prompt | insight:c01-b | insight:c01-b | 53 |
| p-11 | prompt | insight:c05-c | insight:c05-c | 59 |
| p-12 | prompt | insight:c06-c | insight:c06-c | 57 |
| p-13 | prompt | insight:c09-c | insight:c09-c | 50 |
| p-14 | prompt | insight:c14-c | insight:c14-c | 52 |
| p-15 | prompt | insight:r01-fix | insight:r01-fix | 65 |
| p-16 | prompt | — | — | 88 |
| p-17 | prompt | — | — | 56 |
| p-18 | prompt | — | — | 88 |
| p-19 | prompt | — | — | 58 |
| p-20 | prompt | — | — | 65 |
| p-21 | prompt | — | — | 53 |
| p-22 | prompt | — | — | 57 |
| p-23 | prompt | — | insight:h11-crlf-bash | 64 |
| p-24 | prompt | — | — | 52 |
| p-25 | prompt | — | insight:c01-b, insight:p10-h | 60 |
| p-26 | prompt | — | — | 55 |
| p-27 | prompt | — | — | 73 |
| p-28 | prompt | — | — | 74 |
| p-29 | prompt | — | — | 57 |
| p-30 | prompt | — | — | 71 |
| e-01 | post_tool | insight:h08-lnk1181 | insight:h08-lnk1181, insight:h17-lnk1104 | 10 |
| e-02 | post_tool | insight:h09-sqlite-busy | insight:h09-sqlite-busy | 12 |
| e-03 | post_tool | insight:h10-port-in-use | insight:h10-port-in-use | 11 |
| e-04 | post_tool | insight:h11-crlf-bash, insight:h01-sh-lf | insight:h11-crlf-bash, insight:h01-sh-lf | 8 |
| e-05 | post_tool | insight:h12-npm-eresolve | insight:h12-npm-eresolve | 15 |
| e-06 | post_tool | insight:h13-cuda-dll, insight:p03-t | insight:h13-cuda-dll | 9 |
| e-07 | post_tool | insight:h14-exec-policy | insight:h14-exec-policy | 13 |
| e-08 | post_tool | insight:h15-git-nonff, insight:p11-a | insight:h15-git-nonff | 10 |
| e-09 | post_tool | insight:h16-cargo-lock | insight:h16-cargo-lock | 5 |
| e-10 | post_tool | insight:p09-t, insight:p09-h | insight:h04-gha-secrets, insight:h12-npm-eresolve | 9 |
| e-11 | post_tool | insight:h17-lnk1104, insight:c02-b | insight:h17-lnk1104 | 6 |
| e-12 | post_tool | insight:h09-sqlite-busy | insight:h09-sqlite-busy | 10 |
| e-13 | post_tool | — | insight:h17-lnk1104 | 13 |
| e-14 | post_tool | — | insight:p08-t, insight:h15-git-nonff, insight:h04-gha-secrets | 16 |
| e-15 | post_tool | — | — | 6 |
| e-16 | post_tool | — | — | 7 |
| e-17 | post_tool | — | — | 7 |
| e-18 | post_tool | — | insight:h04-gha-secrets, insight:h15-git-nonff | 11 |
| e-19 | post_tool | — | — | 11 |
| e-20 | post_tool | — | — | 4 |
| e-21 | post_tool | — | — | 12 |
| e-22 | post_tool | — | insight:h17-lnk1104 | 10 |
| e-23 | post_tool | — | — | 8 |
| e-24 | post_tool | — | insight:h12-npm-eresolve | 6 |
| t-01 | pre_tool | insight:h01-sh-lf | insight:h01-sh-lf | 27 |
| t-02 | pre_tool | insight:h01-sh-lf | insight:h01-sh-lf | 13 |
| t-03 | pre_tool | insight:h02-ps1-bom | insight:h02-ps1-bom | 13 |
| t-04 | pre_tool | insight:h03-migrations | insight:h03-migrations | 12 |
| t-05 | pre_tool | insight:h04-gha-secrets | insight:h04-gha-secrets | 13 |
| t-06 | pre_tool | insight:h05-force-push | insight:h05-force-push | 14 |
| t-07 | pre_tool | insight:h05-force-push | insight:h05-force-push | 15 |
| t-08 | pre_tool | insight:h06-git-clean | insight:h06-git-clean | 13 |
| t-09 | pre_tool | insight:h07-no-docker | insight:h07-no-docker | 14 |
| t-10 | pre_tool | — | — | 14 |
| t-11 | pre_tool | — | — | 11 |
| t-12 | pre_tool | — | — | 18 |
| t-13 | pre_tool | — | — | 11 |
| t-14 | pre_tool | — | — | 12 |
| t-15 | pre_tool | — | — | 10 |
| t-16 | pre_tool | — | — | 12 |
| t-17 | pre_tool | — | — | 11 |
| t-18 | pre_tool | — | — | 11 |
