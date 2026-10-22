#!/usr/bin/env python3
"""Запуск: python tools/eval/evidence-split-check.py target/eval/<прогон>/data [queries.toml].
Сколько evidence запросов chunk целиком помещается в собственный текст какого-либо листа
(верхняя граница chunk-recall@5) при нарезке прогона. Аргумент — каталог данных прогона."""
import sys
sys.stdout.reconfigure(encoding="utf-8")
import glob, sqlite3, sys, tomllib, pathlib

data = pathlib.Path(sys.argv[1])
db = glob.glob(str(data / 'users' / '*.db'))[0]
c = sqlite3.connect(f'file:{db}?mode=ro', uri=True)
rows = c.execute("""SELECT c.node_id, c.heading_path, c.text, n.path FROM chunks c JOIN nodes n ON n.id=c.node_id
 WHERE c.level='chunk' AND n.deleted_at IS NULL AND c.rev=n.current_rev AND n.path LIKE '%/long/%'
 ORDER BY c.node_id, c.ordinal""").fetchall()


def ov(prev, cur):
    for l in range(min(len(prev), len(cur)), 9, -1):
        if prev.endswith(cur[:l]):
            return l
    return 0


leaves = {}
prev = None
for node, hp, text, path in rows:
    cut = ov(prev[2], text) if prev and prev[0] == node and prev[1] == hp else 0
    leaves.setdefault(path, []).append((' '.join(text[cut:].split()), ' '.join(text.split())))
    prev = (node, hp, text)

q = tomllib.loads(pathlib.Path(sys.argv[2] if len(sys.argv) > 2 else 'eval/queries.toml').read_text(encoding='utf-8'))['query']
split = whole_overlap_only = 0
n = 0
for x in q:
    if x['kind'] != 'chunk':
        continue
    n += 1
    ev = ' '.join(x['evidence'].split())
    ls = leaves[x['expect'][0]]
    core = any(ev in a for a, _ in ls)
    full = any(ev in b for _, b in ls)
    if not core:
        split += 1
        if full:
            whole_overlap_only += 1
        print('разрезано:', x['id'], 'целиком с перекрытием' if full else '')
print(f'chunk-запросов {n}, evidence разрезано границей листа {split} (из них целиком только с перекрытием {whole_overlap_only}); листов длинных заметок {sum(map(len, leaves.values()))}')
