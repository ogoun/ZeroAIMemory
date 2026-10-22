#!/usr/bin/env python3
"""Запуск: python tools/eval/attribute-new-distractors.py <новый.json> <прежний.json>.
Атрибуция разности nDCG@10 M3-base против M2: вклад новых дистракторов (длинные заметки,
insight) и остальное. Парный бутстреп по запросам, 2000 выборок (своё зерно: интервалы близки к xtask, но не
совпадают до знака)."""
import sys
sys.stdout.reconfigure(encoding="utf-8")
import json, math, random, sys, collections

new = json.load(open(sys.argv[1], encoding='utf-8'))
old = json.load(open(sys.argv[2], encoding='utf-8'))
oq = {q['id']: q for q in old['queries']}


def ndcg(ranked, expect):
    dcg = sum(1 / math.log2(i + 2) for i, p in enumerate(ranked[:10]) if p in expect)
    ideal = sum(1 / math.log2(i + 2) for i in range(min(len(expect), 10)))
    return dcg / ideal


def is_new(p):
    return p.startswith('insight:') or p.startswith('entry:') or p.split('/')[1:2] == ['long']


def boot(d, n=2000, seed=1):
    r = random.Random(seed)
    m = [sum(r.choice(d) for _ in d) / len(d) for _ in range(n)]
    m.sort()
    return sum(d) / len(d), m[int(0.025 * n)], m[int(0.975 * n)]


d_all, d_filtered, rows = [], [], []
culprits = collections.Counter()
for q in new['queries']:
    o = oq.get(q['id'])
    if not o or o['expect'] != q['expect']:
        continue
    ranked = [h['path'] for h in q['hits']]
    filt = [p for p in ranked if not is_new(p)]
    a, b, c = o['metrics']['ndcg10'], q['metrics']['ndcg10'], ndcg(filt, q['expect'])
    d_all.append(b - a)
    d_filtered.append(c - a)
    if b < a - 1e-9:
        above = []
        for p in ranked:
            if p in q['expect']:
                break
            if is_new(p):
                above.append(p)
        for p in above:
            culprits['insight' if p.startswith('insight:') else 'long'] += 1
        rows.append((q['id'], q['kind'], round(a, 3), round(b, 3), round(c, 3), above[:3]))

print('n', len(d_all))
print('Δ nDCG@10 (new − M2): %.3f [%.3f…%.3f]' % boot(d_all))
print('Δ nDCG@10 без новых дистракторов: %.3f [%.3f…%.3f]' % boot(d_filtered))
print('запросов с падением', len(rows), 'дистракторы выше ожидаемого:', dict(culprits))
for r in rows:
    print(*r)
