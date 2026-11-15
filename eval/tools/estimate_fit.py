"""Подбор оценки токенов без модели `zam_markdown::EstimateCounter` (M6, задача 5).

Признаки — подсерии серий «слова» (как в `crates/zam-markdown/src/chunk.rs`): классы символов
и «кодовые» серии; коэффициенты — медианная регрессия к bge-m3 (линейная программа HiGHS,
взвешенная абсолютная относительная ошибка) с ограничениями «сумма по слою в пределах» и
монотонностью; запас бюджета `m`, `c` — по целым ответам.

Деление подбор / проверка — **группами** (ревью 3, I-1): сценарий ТС-1, сессия ТС-2, файл
заметки eval, зерно синтетики (подбор — зерно 11, проверка — 12); ответы перед делением
дедуплицируются после нормализации id и цифр (точки сетки и повторы почти совпадают).

Запуск (из корня репозитория; нужны numpy, scipy, tokenizers и
`target/models/tokenizers/bge-m3-int8/tokenizer.json` — `cargo xtask fetch-models --test`):

    python -X utf8 eval/tools/estimate_fit.py [--sha SHA] [--runs GLOB ...] [--out DIR] [--fixture PATH]

Исходники (идентификаторы, пути) и заметки eval читаются `git show` на закреплённом коммите
`--sha` (по умолчанию `PINNED_SHA`); файлы прогонов и токенизатор — с SHA-256 в
`estimate_coef.json`. Идентификаторы и ответы, похожие на секреты (`secret_like`), отбрасываются.

`--runs` — JSON dev-прогонов `xtask eval --tokens` (ТС-1: `scenarios`, ТС-2: `sessions`;
holdout отбрасывается); по умолчанию — прогоны задач 1b, 4, 5 в `.tmp/m6/`. Печатает
коэффициенты (тысячные — таблица `ESTIMATE_TABLE`), несмещённость по слоям, `m`, `c` и
критерии на проверочной половине; пишет `estimate_coef.json` в `--out` и фикстуру теста
(`crates/zam-ml/tests/fixtures/estimate_responses.json`) при `--fixture`.
"""
import argparse
import glob
import hashlib
import json
import math
import os
import random
import re
import subprocess
import sys

import numpy as np
import scipy.sparse as sp
from scipy.optimize import linprog
from tokenizers import Tokenizer

sys.stdout.reconfigure(encoding='utf-8')

REPO = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', '..'))
PINNED_SHA = '1c61a1ca498d0ec1a4da1e6a0cd9d1cd9cc9f90b'
DEFAULT_RUNS = [
    '.tmp/m6/task5/grid/*-dev*.json',
    '.tmp/m6/task1b/pilot2/*-dev.json',
    '.tmp/m6/task5/snippet/*-dev.json',
    '.tmp/m6/task4/pilot2/*-dev.json',
]

# ---------- признаки (зеркало chunk.rs)
CODE_P = set('_./\\:#<>()[]{}=-@$')
SUB = ['digit', 'lat', 'lat4', 'lat8', 'up', 'cyr', 'cyr6', 'oth', 'cjk', 'grk', 'kana', 'hang']
MARKS = [(0x0300, 0x036F), (0x0483, 0x0489), (0x0591, 0x05C7), (0x0610, 0x061A), (0x064B, 0x065F),
         (0x0670, 0x0670), (0x06D6, 0x06ED), (0x0900, 0x0DFF), (0x0E00, 0x0EFF), (0x1AB0, 0x1AFF),
         (0x1DC0, 0x1DFF), (0x20D0, 0x20FF), (0xFE20, 0xFE2F)]


def isw(c):
    o = ord(c)
    return c.isalnum() or any(a <= o <= b for a, b in MARKS)


def kind(c):
    if c.isascii() and c.isdigit():
        return 0
    if c.isascii():
        return 1
    o = ord(c)
    if 0x0400 <= o <= 0x04FF:
        return 2
    if 0x3040 <= o <= 0x30FF:
        return 6
    if 0xAC00 <= o <= 0xD7AF:
        return 7
    if 0x2E80 <= o <= 0x9FFF or 0xF900 <= o <= 0xFAFF or 0xFF00 <= o <= 0xFFEF:
        return 4
    if 0x0370 <= o <= 0x03FF or 0x1F00 <= o <= 0x1FFF:
        return 5
    return 3


def subruns(run):
    out, start = [], 0
    for i in range(1, len(run)):
        a, c = run[i - 1], run[i]
        nxt = run[i + 1] if i + 1 < len(run) else ''
        if kind(a) != kind(c) or (c.isupper() and a.islower()) or (
                a.isupper() and c.isupper() and nxt != '' and nxt.islower()):
            out.append(run[start:i])
            start = i
    out.append(run[start:])
    return out


def sk(s):
    k = kind(s[0])
    if k == 0:
        return 'digit'
    if k == 1:
        if len(s) >= 2 and all(c.isupper() for c in s):
            return 'up'
        return 'lat8' if len(s) >= 8 else 'lat4' if len(s) >= 4 else 'lat'
    if k == 2:
        return 'cyr6' if len(s) >= 6 else 'cyr'
    return {4: 'cjk', 5: 'grk', 6: 'kana', 7: 'hang'}.get(k, 'oth')


NAMES = ['punct', 'symb'] + [p + w + k for w in ('w', 'k') for k in SUB for p in ('n_', 'c_')]
IDX = {k: i for i, k in enumerate(NAMES)}


def feats(t):
    f = np.zeros(len(NAMES))
    n, i = len(t), 0
    while i < n:
        c = t[i]
        if isw(c):
            j = i
            while j < n and isw(t[j]):
                j += 1
            subs = subruns(t[i:j])
            prev = t[i - 1] if i > 0 else ' '
            nxt = t[j] if j < n else ' '
            w = 'k' if (len(subs) > 1 or prev in CODE_P or nxt in CODE_P) else 'w'
            for s in subs:
                k = sk(s)
                f[IDX['n_' + w + k]] += 1
                f[IDX['c_' + w + k]] += len(s)
            i = j
            continue
        if not c.isspace():
            f[IDX['punct' if c.isascii() else 'symb']] += 1
        i += 1
    return f


# ---------- данные
def norm(t):
    """Ключ дедупликации: id и цифры обезличены."""
    t = re.sub(r'\b([a-z]_?:?)[0-9A-HJKMNP-TV-Z]{26}\b', r'\1ID', t)
    return re.sub(r'[0-9]+', '0', t)


def dev_answers(globs):
    """(текст, источник, группа) целых ответов dev-прогонов, без повторов после `norm`."""
    out, seen, used = [], set(), []

    def add(t, src, group):
        if t and t.strip() and len(t) > 10:
            k = norm(t)
            if k not in seen:
                seen.add(k)
                out.append((t, src, group))

    for g in globs:
        for p in sorted(glob.glob(os.path.join(REPO, g))):
            if '-vs-' in p:
                continue
            used.append(p)
            d = json.load(open(p, encoding='utf-8'))
            for sc in d.get('scenarios', []):
                if sc.get('split', 'dev') != 'dev':
                    continue
                for path in ('protocol', 'cached', 'naive', 'reference'):
                    for st in (sc.get(path) or {}).get('steps', []):
                        add(st.get('text'), 'ts1:' + st.get('call', '?').split()[0], 'ts1:' + sc['id'])
            for s in d.get('sessions', []):
                if s.get('split', 'dev') != 'dev':
                    continue
                for st in s.get('steps', []):
                    add(st.get('text'), 'ts2:' + st.get('label', '?'), 'ts2:' + s['id'])
    return out, used


PARTS = ['get', 'set', 'User', 'Id', 'And', 'Tenant', 'From', 'Cache', 'Http', 'Client', 'Factory', 'Options',
         'Service', 'Repository', 'Impl', 'Async', 'Handler', 'XML', 'JSON', 'URL', 'Dto', 'Map', 'List', 'Count']
PARTS2 = PARTS + ['V2', 'Ex', 'Base', 'Abstract']
WORDS = {
    'zh': ['数据库', '迁移', '部署', '失败', '缓存', '策略', '并发', '写入', '读取', '服务器', '配置', '文件', '索引', '查询',
           '错误', '日志', '用户', '权限', '网络', '超时'],
    'ja': ['データベース', 'の', '移行', 'が', 'デプロイ', '時', 'に', '失敗', 'しました', 'キャッシュ', 'サーバー', '設定',
           'ファイル', 'を', '確認', 'する', 'エラー', 'ログ'],
    'ko': ['데이터베이스', '마이그레이션이', '배포', '중에', '실패했습니다', '캐시', '무효화', '전략은', '서버', '설정', '파일을',
           '확인', '오류', '로그', '사용자', '권한'],
    'el': ['Η', 'μετάβαση', 'της', 'βάσης', 'δεδομένων', 'απέτυχε', 'κατά', 'την', 'ανάπτυξη', 'σύστημα', 'μνήμη',
           'αρχείο', 'ρύθμιση', 'σφάλμα', 'χρήστης', 'και', 'για', 'διακομιστής', 'εφαρμογή', 'αναζήτηση', 'ευρετήριο',
           'λειτουργία', 'πρόσβαση', 'δικαιώματα', 'συνδέσεις', 'ενημέρωση', 'έκδοση', 'αποτέλεσμα', 'μετά', 'από',
           'όταν', 'δεν', 'είναι', 'πολύ', 'αργή', 'καθυστέρηση', 'αντίγραφο', 'ασφαλείας', 'επαναφορά', 'Ωμέγα',
           'λάμδα', 'σίγμα', 'δέλτα', 'αλφάβητο', 'συμβολοσειρά'],
    'he': ['שלום', 'עולם', 'בדיקת', 'מטמון', 'מסד', 'הנתונים', 'נכשל', 'במהלך', 'ההעברה', 'שרת', 'קובץ', 'הגדרות',
           'שגיאה', 'משתמש'],
    'ar': ['مرحبا', 'بالعالم', 'اختبار', 'التخزين', 'المؤقت', 'فشل', 'ترحيل', 'قاعدة', 'البيانات', 'أثناء', 'النشر',
           'الخادم', 'ملف', 'خطأ', 'المستخدم'],
    'th': ['ภาษาไทย', 'ทดสอบ', 'การแคช', 'การย้าย', 'ฐานข้อมูล', 'ล้มเหลว', 'ระหว่าง', 'เซิร์ฟเวอร์', 'ไฟล์',
           'ข้อผิดพลาด', 'ผู้ใช้'],
    'hi': ['हिन्दी', 'में', 'परीक्षण', 'कैश', 'डेटाबेस', 'माइग्रेशन', 'तैनाती', 'के', 'दौरान', 'विफल', 'रहा', 'सर्वर',
           'फ़ाइल', 'त्रुटि', 'उपयोगकर्ता'],
}
NOSPACE = ('zh', 'ja', 'th')
B32 = '0123456789ABCDEFGHJKMNPQRSTVWXYZ'
MIX = ['Скрипт с CRLF падает', 'Скрипт с CRLF падает в bash', 'Настрой git config core.autocrlf',
       'используй HttpClient вместо WebRequest', 'ошибка E0502 в borrowChecker', 'pg_dump --format=custom быстрее',
       'файл src/api/search/mod.rs › pack_items', 'ключ ZAM_SEARCH__BUDGET_TOKENS=800', 'Сервер 0.0.11-m6 на Windows']


# Похожие на секреты строки (правила zam_markdown::scan_secrets: префиксы провайдеров, JWT) и
# длинные смешанные строки (кандидаты энтропии) — в синтетику и фикстуру не попадают.
SECRET_RE = re.compile(
    r'(?:AKIA|ASIA)[0-9A-Z]{16}|gh[pousr]_[A-Za-z0-9]{20,}|github_pat_\w{20,}|glpat-[\w-]{20,}'
    r'|xox[abprs]-[A-Za-z0-9-]{10,}|sk-(?:ant-)?[\w-]{20,}|AIza[\w-]{30,}|hf_[A-Za-z0-9]{20,}'
    r'|npm_[A-Za-z0-9]{30,}|SG\.[\w-]{16,}|zam_[A-Za-z0-9]{30,}|eyJ[\w-]{7,}\.[\w-]{10,}')


def secret_like(s):
    if SECRET_RE.search(s):
        return True
    return any(len(w) >= 32 and re.search(r'[0-9]', w) and re.search(r'[a-z]', w) and re.search(r'[A-Z]', w)
               for w in re.findall(r'[A-Za-z0-9+/_=-]{32,}', s))


def git(*args):
    return subprocess.run(['git', '-C', REPO, *args], check=True, capture_output=True).stdout


def code_corpus(sha):
    """Идентификаторы и пути исходников на закреплённом коммите `sha` (`git show`)."""
    names = git('ls-tree', '-r', '--name-only', sha).decode('utf-8').splitlines()
    files = sorted(p for p in names if (p.startswith('crates/') and p.endswith('.rs'))
                   or (p.startswith('web/src/') and p.endswith('.ts')))
    idents, paths = set(), []
    for p in files:
        paths.append(p)
        try:
            src = git('show', f'{sha}:{p}').decode('utf-8')
        except UnicodeDecodeError:
            continue
        for m in re.finditer(r'\b[A-Za-z_][A-Za-z0-9_]{3,}\b', src):
            if not secret_like(m.group(0)):
                idents.add(m.group(0))
    return sorted(idents), paths


def corpus_docs(sha):
    """Заметки eval (`eval/corpus`, `eval/sessions-corpus`) на коммите `sha`."""
    names = git('ls-tree', '-r', '--name-only', sha, 'eval/corpus', 'eval/sessions-corpus').decode('utf-8').splitlines()
    files = sorted(p for p in names if p.endswith('.md'))
    return files, [git('show', f'{sha}:{p}').decode('utf-8') for p in files]


def sha256(path):
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        for b in iter(lambda: f.read(1 << 20), b''):
            h.update(b)
    return h.hexdigest()


def synthetic(rng, idents, paths, docs):
    """(текст, источник, слой) синтетики одного зерна."""
    cs_name = lambda: ''.join(rng.sample(PARTS, rng.randint(2, 5)))
    phrase = lambda lang, k: ('' if lang in NOSPACE else ' ').join(rng.choice(WORDS[lang]) for _ in range(k)) + (
        '。' if lang in ('zh', 'ja') else '.')
    rid = lambda: '01M' + ''.join(rng.choice(B32) for _ in range(23))
    out = []
    for _ in range(80):  # ответы карты кода
        lines = ['### Код (карта, ранжированная)']
        for _ in range(rng.randint(5, 60)):
            p = rng.choice(paths) if rng.random() < .6 else 'src/' + '/'.join(
                x.lower() for x in rng.sample(PARTS, 2)) + '/' + cs_name() + rng.choice(['.ts', '.cs', '.java'])
            lines.append(f'- {p} — {rng.choice(idents)}, {cs_name()}::{rng.choice(idents)} ×{rng.randint(1, 40)}')
        out.append(('\n'.join(lines), 'code', 'code'))
    phrases = [phrase(lang, rng.randint(2, 9)) for lang in WORDS for _ in range(25)]
    for _ in range(40):  # выдача с id и фразами письменностей
        out.append(('\n'.join(f'{i + 1}. n:{rid()} ' + ' '.join(rng.choice(phrases) for _ in range(rng.randint(1, 4)))
                              for i in range(rng.randint(3, 15))), 'scripts', 'scripts'))
    out += [(t, 'ident', 'idents') for t in rng.sample(idents, 1500)]
    out += [(f'{p}: fn {rng.choice(idents)} · {rng.choice(idents)} — {rng.randint(0, 899)} ссылок', 'map', 'maplines')
            for p in rng.sample(paths, len(paths) // 2)]
    csjt = []
    for _ in range(750):
        parts = [rng.choice(PARTS2) for _ in range(rng.randint(2, 6))]
        name = ''.join(parts)
        csjt.append(f"src/{'/'.join(p.lower() for p in parts[:3])}/{name}{rng.choice(['.ts', '.cs', '.java', '.tsx'])}")
        csjt.append(name)
    out += [(t, 'csjt', 'csjt') for t in csjt]
    for _ in range(200):
        words = rng.choice(docs).split()[:40] or ['x']
        out.append((f'{rng.choice(words)} {rng.choice(csjt)} {rng.choice(idents)}', 'mix', 'mixed'))
    for lang in WORDS:
        out += [(phrase(lang, rng.randint(1, 12)), 'scr', 'scripts_' + lang) for _ in range(75)]
    for d in rng.sample(docs, len(docs) // 2):
        title = (d.splitlines() or [''])[0][:60]
        snip = d[100:340].replace('\n', ' ')
        out.append((f"1. n:{rid()} Knowledge/ru/x/{title.replace(' ', '-')} › Причина  [.82] r7 · 2026-10-10 14:03:22 · 340t\n   {snip}\n"
                    f"2. i:{rid()} insight/gotcha «{title}»  [.77] hyp · 2026-09-28 08:00:01 · 120t\n", 'card', 'cards'))
    return out


def answer_layer(t):
    f = feats(t)
    chars = max(1, len(t))
    other = sum(f[IDX[p + w + k]] for p in ('c_',) for w in ('w', 'k') for k in ('oth', 'cjk', 'grk', 'kana', 'hang'))
    if other > 0.2 * chars:
        return 'scripts'
    if len(re.findall(r'[a-z]:[0-9A-Z]{26}', t)) * 26 > 0.06 * chars:
        return 'idheavy'
    return 'prose'


def half_of(key):
    return int(hashlib.blake2b(key.encode(), digest_size=2).hexdigest(), 16) % 2


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--runs', nargs='*', default=DEFAULT_RUNS)
    ap.add_argument('--out', default=os.path.join(REPO, '.tmp', 'm6', 'task5', 'diag'))
    ap.add_argument('--fixture')
    ap.add_argument('--sha', default=PINNED_SHA)
    ap.add_argument('--tokenizer', default=os.path.join(REPO, 'target', 'models', 'tokenizers', 'bge-m3-int8', 'tokenizer.json'))
    a = ap.parse_args()
    tok = Tokenizer.from_file(a.tokenizer)
    bge = lambda t: len(tok.encode(t, add_special_tokens=False).ids)

    real, used = dev_answers(a.runs)
    groups = sorted({g for _, _, g in real})
    print('ответов dev после дедупликации', len(real), '· групп', len(groups))
    idents, paths = code_corpus(a.sha)
    doc_files, docs = corpus_docs(a.sha)

    # (текст, источник, слой, половина)
    samples = [(t, s, answer_layer(t), half_of(g)) for t, s, g in real]
    samples += [(d, 'doc', 'docs', half_of(p)) for d, p in zip(docs, doc_files)]
    samples += [(p, 'path', 'paths', half_of(p)) for p in paths]
    for h, seed in ((0, 11), (1, 12)):
        samples += [(t, s, l, h) for t, s, l in synthetic(random.Random(seed), idents, paths, docs)]
    samples += [(t, 'mix', 'mixed', half_of(t)) for t in MIX]
    n0 = len(samples)
    samples = [x for x in samples if not SECRET_RE.search(x[0])]
    print('отброшено похожих на секреты текстов:', n0 - len(samples))

    X = np.array([feats(t) for t, *_ in samples])
    y = np.array([bge(t) for t, *_ in samples], float)
    LANG = np.array([l for _, _, l, _ in samples])
    L = np.array([('scripts_w' if l.startswith('scripts_') else l) for l in LANG])
    fit = np.array([h == 0 for *_, h in samples])
    layers = sorted(set(L))
    print('подбор / проверка по слоям:', {l: (int(((L == l) & fit).sum()), int(((L == l) & ~fit).sum())) for l in layers})

    # ---------- медианная регрессия (ЛП) с ограничениями
    MAIN = ('prose', 'idheavy', 'code', 'scripts', 'docs', 'idents')
    A, b = [], []  # A·c ≤ b

    def between(m, lo, hi):
        S, Y = X[m].sum(0), y[m].sum()
        A.append(-S); b.append(-lo * Y)
        A.append(S); b.append(hi * Y)

    for l in layers:
        between((L == l) & fit, *((1.00, 1.05) if l in MAIN else (0.97, 1.05)))
    for l in sorted(set(LANG)):
        if l.startswith('scripts_'):
            between((LANG == l) & fit, 0.95, 1.06)

    def ge(pa, pb):  # pa·c ≥ pb·c
        d = np.zeros(len(NAMES))
        for k, x in pa:
            d[IDX[k]] += x
        for k, x in pb:
            d[IDX[k]] -= x
        A.append(-d); b.append(0.0)

    for k in SUB:  # «кодовая» подсерия не дешевле такой же в прозе
        ge([('n_k' + k, 1)], [('n_w' + k, 1)])
        ge([('c_k' + k, 1)], [('c_w' + k, 1)])
    for wv in ('w', 'k'):  # монотонность на границах длины
        ge([('n_' + wv + 'lat4', 1), ('c_' + wv + 'lat4', 4)], [('n_' + wv + 'lat', 1), ('c_' + wv + 'lat', 3)])
        ge([('n_' + wv + 'lat8', 1), ('c_' + wv + 'lat8', 8)], [('n_' + wv + 'lat4', 1), ('c_' + wv + 'lat4', 7)])
        ge([('n_' + wv + 'cyr6', 1), ('c_' + wv + 'cyr6', 6)], [('n_' + wv + 'cyr', 1), ('c_' + wv + 'cyr', 5)])
        ge([('n_' + wv + 'up', 1), ('c_' + wv + 'up', 2)], [('n_' + wv + 'lat', 1), ('c_' + wv + 'lat', 1)])
    bounds = [(0, None)] * len(NAMES)
    for k in ('c_wcjk', 'c_kcjk', 'c_wgrk', 'c_kgrk', 'c_woth', 'c_koth'):
        bounds[IDX[k]] = (0.15, None)  # незнакомые письменности — не даром
    Xf, yf = X[fit], y[fit]
    wf = 1.0 / np.maximum(yf, 1)
    K, N = len(NAMES), len(yf)
    res = linprog(np.concatenate([np.zeros(K), wf, wf]),
                  A_ub=sp.hstack([sp.csr_matrix(np.array(A)), sp.csr_matrix((len(A), 2 * N))]), b_ub=np.array(b),
                  A_eq=sp.hstack([sp.csr_matrix(Xf), -sp.eye(N), sp.eye(N)]), b_eq=yf,
                  bounds=bounds + [(0, None)] * (2 * N), method='highs')
    print('ЛП:', res.status, res.message)
    if res.status != 0:
        raise SystemExit(1)
    C = np.array([math.floor(v * 1000 + 0.5) / 1000 for v in res.x[:K]])
    coef = {k: float(v) for k, v in zip(NAMES, C) if v}
    print('коэффициенты (тысячные — ESTIMATE_TABLE):', {k: int(round(v * 1000)) for k, v in coef.items()})
    E = X @ C
    print('несмещённость (оценка / bge-m3) — подбор / проверка:')
    for l in layers:
        f_, c_ = (L == l) & fit, (L == l) & ~fit
        print(f'  {l:10} {E[f_].sum() / y[f_].sum():.3f} / {E[c_].sum() / max(1, y[c_].sum()):.3f}  (n {f_.sum()}/{c_.sum()})')
    for l in sorted(set(LANG)):
        if l.startswith('scripts_'):
            f_, c_ = (LANG == l) & fit, (LANG == l) & ~fit
            print(f'  {l:10} {E[f_].sum() / y[f_].sum():.3f} / {E[c_].sum() / max(1, y[c_].sum()):.3f}')

    # ---------- запас бюджета на целых ответах подбора
    R = np.isin(L, ('prose', 'idheavy', 'code', 'scripts'))
    mm = 0
    for l in ('prose', 'idheavy', 'code', 'scripts'):
        f_ = (L == l) & fit
        q = np.percentile(y[f_] / np.maximum(E[f_], 1e-9), 99)
        print(f'  p99 bge/оценка {l}: {q:.3f}')
        mm = max(mm, q)
    m = math.ceil(mm * 100) / 100
    short = R & fit & (y < 150)
    c = max(0, math.ceil(np.percentile(y[short] - m * E[short], 99)))
    print('m', m, 'c', c)
    # критерии — как в тесте: наименьший целый бюджет B, который пропускает fits
    cap_milli = round(m * 1000)
    Eint = np.floor(E + 0.5)
    B = np.ceil(Eint * cap_milli / 1000) + c
    for name, sel in (('подбор', R & fit), ('проверка', R & ~fit)):
        over = y[sel] > B[sel]
        exc = (y[sel] - B[sel]) / B[sel]
        band = sel & (y >= 400) & (y <= 2000)
        print(f'{name}: ответов {sel.sum()}, превышений {over.mean() * 100:.2f} %, макс {max(0, exc.max()) * 100:.1f} %, '
              f'недогрузка (400–2000, n {band.sum()}) {(1 - y[band] / B[band]).mean() * 100:.1f} %')
    os.makedirs(a.out, exist_ok=True)
    inputs = {
        'sha': a.sha,
        'tokenizer': {'path': os.path.relpath(a.tokenizer, REPO).replace(os.sep, '/'), 'sha256': sha256(a.tokenizer)},
        'runs': [{'path': os.path.relpath(p, REPO).replace(os.sep, '/'), 'sha256': sha256(p)} for p in used],
    }
    json.dump({'coef': coef, 'm': m, 'c': c, 'names': NAMES, 'inputs': inputs},
              open(os.path.join(a.out, 'estimate_coef.json'), 'w', encoding='utf-8'), indent=1, ensure_ascii=False)
    if a.fixture:  # проверочная половина целых ответов, тексты целиком
        held = [(t, s, l) for (t, s, l, _), r, f_ in zip(samples, R, fit) if r and not f_]
        rng = random.Random(5)
        rng.shuffle(held)
        fx = []
        for lay, n in (('prose', 70), ('idheavy', 70), ('code', 40), ('scripts', 40)):
            fx += [{'layer': l, 'source': s, 'text': t} for t, s, l in held if l == lay][:n]
        json.dump(fx, open(a.fixture, 'w', encoding='utf-8'), ensure_ascii=False)
        print('фикстура', len(fx), 'ответов,', round(os.path.getsize(a.fixture) / 1024), 'КиБ')


if __name__ == '__main__':
    main()
