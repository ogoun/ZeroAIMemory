"""Индекс итогового отчёта замера M6 (задача 13b): `eval/results/<дата>-m6-<модель>-tokens.{json,md}`.

Своей статистики нет: значения критериев **берутся** из файлов сравнений `cargo xtask eval`
(ТС-2 — `--sessions … --compare`, ТС-1 — `--tokens --via mcp --arm … --compare …`, качество —
`--compare` прогонов `--mode`), проверяются пороги предрегистрации (журнал этапа, «Задача 13b —
предрегистрация») и пишутся ссылки на файлы с sha256. Тесты `m6_acceptance`
(`acceptance_ts1_vs_b4`, `acceptance_ts2_vs_b56`, переменная `ZAM_M6_REPORT`) читают этот индекс и
сверяют пороги по самим файлам сравнений.

Запуск (из корня репозитория):

    python -X utf8 eval/tools/m6_report.py <конфиг.json> --out eval/results/<дата>-m6-bge-m3-fp16-tokens

Конфиг — пути (от корня репозитория) по ролям:

    {"date": "2026-11-14", "model": "bge-m3-fp16",
     "ts2": {"<агент>": {"m6": arm.json, "b56": arm.json, "b57": arm.json, "aa": arm.json?,
                         "vs_b56": cmp.json, "vs_b57": cmp.json, "aa_vs_m6": cmp.json?,
                         "attempt": 1, "idea_hint_exceptions": {"count": 0, "events": []}?}},
     "ts1": {"cuda": cmp-mean.json, "cpu": cmp.json, "dev_aa": cmp.json?,
             "control": {"verdict": "ok" | "<расхождения>", "files": [a.json, b.json]}},
     "quality": [{"name": "...", "cmp": run.json, "base": base.json}, ...],
     "hook_budgets": {"verdict": "...", "log": путь?},
     "files": {"<роль>": путь, ...}}
"""
import argparse
import hashlib
import json
import os
import sys

# пороги предрегистрации 13b (спецификация M6 §17 п. 1, 2, 4; решение владельца 2026-11-08)
TS2_R_FULL_MAX = 1.15
TS2_R_NORULES_MAX = 1.00
TS2_D_HINT_MIN = -0.02
TS1_D_MICRO_MIN = -0.02
TS1_L_MAX = 1
TS1_R_HI_MAX = 1.00
TS1_R_GOAL = 0.70
Q_D_MIN = -0.01


def sha256(path):
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        for b in iter(lambda: f.read(1 << 20), b''):
            h.update(b)
    return h.hexdigest()


def load(path):
    with open(path, encoding='utf-8') as f:
        return json.load(f)


def part(cmp, name, key='parts'):
    for p in cmp.get(key) or []:
        if p.get('part') == name:
            return p
    return None


def est(e):
    return None if e is None else {k: e.get(k) for k in ('mean', 'lo', 'hi')}


def rel_path(p):
    """Путь из файла `xtask eval` — от корня репозитория (абсолютный путь машины в отчёт не идёт)."""
    if not p or not os.path.isabs(p):
        return p
    try:
        r = os.path.relpath(p, os.getcwd())
    except ValueError:
        return p
    return p if r.startswith('..') else r.replace(os.sep, '/')


def share(t):
    return None if not t or not t.get('total') else t['found'] / t['total']


def required_by_profile(arm):
    """Обязательные строки плеча по профилям: (найдено, всего) по сессиям всех повторов."""
    out = {}
    for s in arm.get('sessions', []):
        r = s.get('required') or []
        found = sum(1 for x in r if x.get('found'))
        p = out.setdefault(s.get('profile') or '?', [0, 0])
        p[0] += found
        p[1] += len(r)
    return {k: {'found': f, 'total': t} for k, (f, t) in out.items()}


def idea_match_ok(match, exceptions):
    """Совпадение подсказок-идей с B5.7: все события, кроме расхождений по стоп-основам."""
    if not match or not match.get('total'):
        return True
    n = (exceptions or {}).get('count', 0)
    return match['found'] + n == match['total']


def ts2_block(agent, c, files):
    m6, b56, b57 = load(c['m6']), load(c['b56']), load(c['b57'])
    v56, v57 = load(c['vs_b56']), load(c['vs_b57'])
    for role in ('m6', 'b56', 'b57', 'aa', 'vs_b56', 'vs_b57', 'aa_vs_m6'):
        if c.get(role):
            files[f'ts2.{agent}.{role}'] = c[role]
    std56 = part(v56, 'standard')
    std57_nr = part(v57, 'standard', 'parts_no_rules')
    all56 = part(v56, 'всего')
    misses = {}
    arms = {'m6': m6, 'b56': b56, 'b57': b57}
    if c.get('aa'):
        arms['aa'] = load(c['aa'])
    for k, a in arms.items():
        s = a['summary']
        misses[k] = {'reps_with_misses': s.get('reps_with_misses', []),
                     'hook_budget_misses': s.get('hook_budget_misses', 0),
                     'bad_outputs': s.get('bad_outputs', 0)}
    req = required_by_profile(m6)
    worst = min((share(v) for v in (m6['summary'].get('required_by_rep') or {}).values()),
                default=None)
    noise = None
    if c.get('aa_vs_m6'):
        noise = load(c['aa_vs_m6']).get('abs_rel_diff_p95')
    out = {
        'date_utc': {k: a['info'].get('date') for k, a in arms.items()},
        'hook_agent': {k: a['info'].get('hook_agent') for k, a in arms.items()},
        'device': m6['info'].get('device'),
        'repeat': {k: a['info'].get('repeat') for k, a in arms.items()},
        'attempt': c.get('attempt', 1),
        'r_full_vs_b56': {'standard': est(std56['total']['r']) if std56 else None,
                          'all': est(all56['total']['r']) if all56 else None},
        'r_norules_vs_b57': {'standard': est(std57_nr['total']['r']) if std57_nr else None},
        'd_hint_recall': est(all56.get('d_hint_recall')) if all56 else None,
        'd_hint_recall_strict': est(all56.get('d_hint_recall_strict')) if all56 else None,
        'required_m6': {'by_profile': req, 'worst_rep_share': worst},
        'idea_hint_recall_m6': m6['summary'].get('idea_hint_recall'),
        'idea_hints_match_b57': v57.get('idea_hints_match'),
        'misses': misses,
        'noise_aa_p95': noise,
        # расхождения подсказок-идей с B5.7 по правилу стоп-основ задачи 16 (предрегистрация 13b):
        # {"count": N, "events": [...]} — разбор по текстам ответов, вносится в конфиг
        'idea_hint_exceptions': c.get('idea_hint_exceptions'),
    }
    rf = out['r_full_vs_b56']['standard']
    rn = out['r_norules_vs_b57']['standard']
    dh, dhs = out['d_hint_recall'], out['d_hint_recall_strict']
    checks = {
        'r_full_standard_le_1_15': rf is not None and rf['mean'] <= TS2_R_FULL_MAX,
        'r_norules_standard_vs_b57_le_1_00': rn is not None and rn['mean'] <= TS2_R_NORULES_MAX,
        'required_1_0': all(v['found'] == v['total'] for v in req.values()) and worst == 1.0,
        'd_hint_recall_lo_ge_-0_02': dh is not None and dh['lo'] >= TS2_D_HINT_MIN,
        'd_hint_recall_strict_lo_ge_-0_02': dhs is not None and dhs['lo'] >= TS2_D_HINT_MIN,
        'idea_hint_recall_1_0': share(out['idea_hint_recall_m6']) in (None, 1.0),
        'idea_hints_match_b57': idea_match_ok(out['idea_hints_match_b57'],
                                              out['idea_hint_exceptions']),
        'no_reps_with_misses': all(not v['reps_with_misses'] for v in misses.values()),
        'same_utc_day': len(set(out['date_utc'].values())) == 1,
    }
    out['checks'] = checks
    return out


def ts1_part(c, files):
    out = {}
    if c.get('cuda'):
        files['ts1.cuda'] = c['cuda']
        r = load(c['cuda'])
        out['cuda'] = {k: r.get(k) for k in (
            'arm', 'base', 'split', 'scenarios', 'excluded', 'base_tokens_mean', 'arm_tokens_mean',
            'r', 'base_micro', 'arm_micro', 'd_micro', 'net_losses', 'net_losses_all_reps',
            'noninferior', 'noise_base_p95', 'noise_arm_p95', 'pairs', 'l0_excluded')}
        x = out['cuda']
        out['cuda']['checks'] = {
            'd_micro_lo_ge_-0_02': x['d_micro'] is not None and x['d_micro']['lo'] >= TS1_D_MICRO_MIN,
            'net_losses_le_1': x['net_losses'] <= TS1_L_MAX,
            'r_hi_lt_1_00': x['r'] is not None and x['r']['hi'] < TS1_R_HI_MAX,
            'goal_r_le_0_70': x['r'] is not None and x['r']['mean'] <= TS1_R_GOAL,
        }
    if c.get('cpu'):
        files['ts1.cpu'] = c['cpu']
        r = load(c['cpu'])
        out['cpu'] = {k: r.get(k) for k in (
            'arm', 'base', 'split', 'scenarios', 'base_tokens', 'arm_tokens', 'r', 'base_micro',
            'arm_micro', 'd_micro', 'net_losses', 'noninferior', 'l0_excluded')}
        x = out['cpu']
        out['cpu']['checks'] = {
            'd_micro_lo_ge_-0_02': x['d_micro'] is not None and x['d_micro']['lo'] >= TS1_D_MICRO_MIN,
            'net_losses_le_1': x['net_losses'] <= TS1_L_MAX,
        }
    if c.get('dev_aa'):
        files['ts1.dev_aa'] = c['dev_aa']
        r = load(c['dev_aa'])
        out['dev_aa'] = {k: r.get(k) for k in ('arm', 'base', 'r', 'd_micro', 'net_losses')}
    if c.get('control'):
        out['control_v09_eq_v010'] = c['control']
        for i, f in enumerate(c['control'].get('files', [])):
            files[f'ts1.control.{i}'] = f
    return out


def quality_part(items, files):
    out = []
    for q in items:
        files[f'quality.{q["name"]}'] = q['cmp']
        if q.get('base'):
            files[f'quality.{q["name"]}.base'] = q['base']
        r = load(q['cmp'])
        cmp = r.get('compare') or {}
        m = r.get('metrics', {}).get('overall', {})
        # Δ — `xtask/src/eval/compare.rs` (парный бутстреп запросов, двусторонние 95 %: нижняя
        # граница строже односторонней)
        entry = {'name': q['name'], 'mode': r.get('info', {}).get('mode'),
                 'ndcg10': est(m.get('ndcg10')), 'recall5': est(m.get('recall5')),
                 'reliable_first': est(m.get('reliable_first')),
                 'compare': {k: rel_path(cmp.get(k)) if k == 'against' else cmp.get(k)
                             for k in ('against', 'n', 'd_ndcg10', 'd_recall5', 'regression')}
                 if cmp else None}
        d1 = (cmp or {}).get('d_ndcg10')
        d2 = (cmp or {}).get('d_recall5')
        entry['checks'] = {
            'd_ndcg10_lo_ge_-0_01': d1 is not None and d1.get('lo', -1) >= Q_D_MIN,
            'd_recall5_lo_ge_-0_01': d2 is not None and d2.get('lo', -1) >= Q_D_MIN,
        }
        out.append(entry)
    return out


def md(rep):
    L = [f"# Итоговый замер M6 — {rep['date']} ({rep['model']})", '',
         'Индекс файлов сравнений `cargo xtask eval` и значения критериев предрегистрации задачи '
         '13b (журнал этапа). Своей статистики нет: числа — из файлов ниже.', '']
    for agent, b in rep.get('ts2', {}).items():
        L.append(f'## ТС-2, агент хуков {agent} (попытка {b["attempt"]})')
        L.append('')
        rf, rn = b['r_full_vs_b56']['standard'], b['r_norules_vs_b57']['standard']
        f = lambda e: '—' if not e else f"{e['mean']:.3f} [{e['lo']:.3f}–{e['hi']:.3f}]"
        L.append(f"- R полный M6/B5.6, standard: {f(rf)} (порог точечной ≤ {TS2_R_FULL_MAX})")
        L.append(f"- R без Rules M6/B5.7, standard: {f(rn)} (порог ≤ {TS2_R_NORULES_MAX})")
        L.append(f"- R полный M6/B5.6, всего: {f(b['r_full_vs_b56']['all'])}")
        L.append(f"- Δ hint_recall: {f(b['d_hint_recall'])}; строго: {f(b['d_hint_recall_strict'])}")
        L.append(f"- обязательные строки M6: {b['required_m6']}")
        L.append(f"- подсказки-идеи M6: {b['idea_hint_recall_m6']}; совпадение с B5.7: "
                 f"{b['idea_hints_match_b57']}")
        L.append(f"- промахи: {b['misses']}")
        L.append(f"- порог шума A/A (p95 |Δ|/база): {b['noise_aa_p95']}")
        L.append(f"- проверки: {b['checks']}")
        L.append('')
    t = rep.get('ts1', {})
    if 'cuda' in t:
        x = t['cuda']
        L.append('## ТС-1 к B4, CUDA fp16, по среднему на сценарий')
        L.append('')
        for k in ('arm', 'base', 'scenarios', 'excluded', 'r', 'base_micro', 'arm_micro', 'd_micro',
                  'net_losses', 'net_losses_all_reps', 'noise_base_p95', 'noise_arm_p95', 'checks'):
            L.append(f'- {k}: {x.get(k)}')
        L.append('')
    if 'cpu' in t:
        L.append('## ТС-1 к B4, int8 CPU (контроль)')
        L.append('')
        for k, v in t['cpu'].items():
            L.append(f'- {k}: {v}')
        L.append('')
    if 'control_v09_eq_v010' in t:
        L.append(f"- контроль v0.0.9 ≡ v0.0.10-m5.7 (FTS, holdout): {t['control_v09_eq_v010']}")
        L.append('')
    if rep.get('quality'):
        L.append('## Качество к v0.0.9')
        L.append('')
        for q in rep['quality']:
            L.append(f"- {q['name']} ({q['mode']}): nDCG@10 {q['ndcg10']}, recall@5 {q['recall5']}, "
                     f"reliable_first {q['reliable_first']}, сравнение {q['compare']}, "
                     f"проверки {q['checks']}")
        L.append('')
    if rep.get('hook_budgets'):
        L.append(f"## §17 п. 15\n\n- {rep['hook_budgets']}\n")
    L.append('## Файлы')
    L.append('')
    for f in rep['files']:
        L.append(f"- `{f['role']}` — `{f['path']}` ({f['bytes']} байт, sha256 `{f['sha256'][:16]}…`)")
    return '\n'.join(L) + '\n'


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('config')
    ap.add_argument('--out', required=True, help='префикс пути без расширения')
    a = ap.parse_args()
    cfg = load(a.config)
    files = dict(cfg.get('files', {}))
    rep = {'date': cfg['date'], 'model': cfg.get('model', 'bge-m3-fp16'), 'thresholds': {
        'ts2_r_full_standard_max': TS2_R_FULL_MAX, 'ts2_r_norules_standard_vs_b57_max':
        TS2_R_NORULES_MAX, 'ts2_d_hint_lo_min': TS2_D_HINT_MIN, 'ts1_d_micro_lo_min':
        TS1_D_MICRO_MIN, 'ts1_net_losses_max': TS1_L_MAX, 'ts1_r_hi_max': TS1_R_HI_MAX,
        'ts1_r_goal': TS1_R_GOAL, 'quality_d_lo_min': Q_D_MIN}}
    if cfg.get('ts2'):
        rep['ts2'] = {agent: ts2_block(agent, c, files) for agent, c in cfg['ts2'].items()}
    if cfg.get('ts1'):
        rep['ts1'] = ts1_part(cfg['ts1'], files)
    if cfg.get('quality'):
        rep['quality'] = quality_part(cfg['quality'], files)
    if cfg.get('hook_budgets'):
        rep['hook_budgets'] = cfg['hook_budgets']
    rep['files'] = [{'role': r, 'path': p.replace(os.sep, '/'), 'bytes': os.path.getsize(p),
                     'sha256': sha256(p)} for r, p in sorted(files.items())]
    with open(a.out + '.json', 'w', encoding='utf-8', newline='\n') as f:
        json.dump(rep, f, ensure_ascii=False, indent=2)
        f.write('\n')
    with open(a.out + '.md', 'w', encoding='utf-8', newline='\n') as f:
        f.write(md(rep))
    print(a.out + '.json')
    print(a.out + '.md')


if __name__ == '__main__':
    sys.exit(main())
