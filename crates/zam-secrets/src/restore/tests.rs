use super::*;
use crate::mask_text;

/// Ключ из частей (в исходнике целиком его нет — push protection); `n` — отличие ключей.
fn key(n: u32) -> String {
    format!("gh{}_{}{n:04}", "p", "aB3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1eF")
}

/// Пароль ключ-значения из частей.
fn pw(n: u32) -> String {
    format!("{}{n}{}", "hunter", "Qz9x")
}

const M: &str = SECRET_MARK;

/// Метка с номером.
fn m(n: u32) -> String {
    format!("‹secret-{n}›")
}

/// Все метки (с номером и без) — `‹secret›`.
fn plain(s: &str) -> String {
    let mut out = String::new();
    let mut pos = 0;
    for (r, _) in crate::marks(s) {
        out.push_str(&s[pos..r.start]);
        out.push_str(M);
        pos = r.end;
    }
    out.push_str(&s[pos..]);
    out
}

/// Восстановление строго по номерам: ожидаемый текст, без меток без пары и отказа.
#[track_caller]
fn exact(base: &str, edited: &str, want: &str) {
    let r = restore(base, edited);
    assert_eq!(r.text, want, "правка {edited:?}");
    assert_eq!((r.kept, r.exposed.len()), (0, 0), "правка {edited:?}");
}

/// Вид: маски — по порядку `‹secret-1›`, `‹secret-2›`, …; без номеров вид — ровно `mask_text`;
/// литералы меток исходника (`‹secret›`, `‹secret-N›`) — `‹secret›` без номера.
#[test]
fn numbered_view_order() {
    let (k1, k2) = (key(1), key(2));
    let base = format!(
        "# K\ntoken: {k1}\nbase: https://user:{}@db.example.com/\nb: {k2}\n",
        pw(1)
    );
    let v = numbered_view(&base);
    assert_eq!(
        v,
        format!(
            "# K\ntoken: {}\nbase: https://user:{}@db.example.com/\nb: {}\n",
            m(1),
            m(2),
            m(3)
        )
    );
    assert_eq!(plain(&v), mask_text(&base));
    assert!(matches!(numbered_view("без секретов"), Cow::Borrowed(_)));
    let lit = format!("было {M} и {} и token: {k1}\n", m(7));
    assert_eq!(
        numbered_view(&lit),
        format!("было {M} и {M} и token: {}\n", m(1))
    );
    // поле из нескольких строк — сквозная нумерация
    let mut f = FieldMasks::new();
    let (a, b) = (format!("a {k1}"), format!("b {k2}"));
    assert_eq!(f.view(&a), format!("a {}", m(1)));
    assert_eq!(f.view("c"), "c");
    assert_eq!(f.view(&b), format!("b {}", m(2)));
    assert_eq!(
        (f.len(), f.value(2), f.value(3)),
        (2, Some(k2.as_str()), None)
    );
    assert_eq!(f.value(0), None);
}

/// Вид без правки — исходник; правка вокруг метки, перестановка, перенос, удаление строк —
/// значения по номерам.
#[test]
fn restore_by_number() {
    let (k1, k2) = (key(11), key(12));
    let base = format!("# K\n\na: {k1}\nb: {k2}\n\nхвост\n");
    let v = numbered_view(&base).into_owned();
    exact(&base, &v, &base);
    exact(
        &base,
        &format!("# K\n\nb: {} (prod)\nтекст\na: {}\n", m(2), m(1)),
        &format!("# K\n\nb: {k2} (prod)\nтекст\na: {k1}\n"),
    );
    exact(
        &base,
        &format!("хвост {}\n", m(2)),
        &format!("хвост {k2}\n"),
    );
    // удалены обе — значений нет
    exact(&base, "# K\n", "# K\n");
    // повтор номера — повтор значения
    exact(
        &base,
        &format!("a: {}\nкопия: {}\n", m(1), m(1)),
        &format!("a: {k1}\nкопия: {k1}\n"),
    );
}

/// Метка без номера и номер, которого в базе нет, — как есть и `kept`; база без секретов — все
/// метки как есть; литерал базы напротив себя — тоже `kept`.
#[test]
fn restore_unpaired_kept() {
    let k = key(21);
    let base = format!("token: {k}\n");
    let r = restore(
        &base,
        &format!("token: {}\nx {M}\ny {}\nz {}\n", m(1), m(2), m(0)),
    );
    assert_eq!(
        r.text,
        format!("token: {k}\nx {M}\ny {}\nz {}\n", m(2), m(0))
    );
    assert_eq!((r.kept, r.exposed.len()), (3, 0));
    let r = restore("чистый текст\n", &format!("чистый текст {}\n", m(1)));
    assert_eq!((r.text, r.kept), (format!("чистый текст {}\n", m(1)), 1));
    let lit = format!("сервис: {M}\n");
    let r = restore(&lit, &numbered_view(&lit));
    assert_eq!((r.text.as_str(), r.kept), (lit.as_str(), 1));
    assert_eq!(restore(&base, "без меток").text, "без меток");
    assert!(has_mark(&m(3)) && has_mark(M) && !has_mark("‹secret-›") && !has_mark("secret"));
    assert_eq!(marks_in(&format!("{M}{}{M} {}", m(1), m(12))), 4);
}

/// Пробы ревью кругов 1–2 (S1, S2, V1–V7, db1/db3): значение — строго по номеру, который агент
/// оставил у строки, без подмен и молчаливой потери; номер другой строки — её значение (явный
/// выбор агента).
#[test]
fn restore_review_cases() {
    let (a, b) = (key(1), key(2));
    let (p1, p2, p3) = (pw(1), pw(2), pw(3));
    // S1: секция Prod удалена, строка Dev правлена словами строки Prod
    let base = format!("# K\n## Prod\ntoken: {a} (prod)\n## Dev\ntoken: {b}\n");
    exact(
        &base,
        &format!("# K\n## Dev\ntoken: {} (было prod)\n", m(2)),
        &format!("# K\n## Dev\ntoken: {b} (было prod)\n"),
    );
    // V1: то же, строка встала после неизменённой строки
    let base1 = format!("# K\n## Prod\ntoken: {a} (prod)\n## Dev\ntoken: {b}\nтекст\n");
    exact(
        &base1,
        &format!("# K\n## Dev\nтекст\ntoken: {} (было prod)\n", m(2)),
        &format!("# K\n## Dev\nтекст\ntoken: {b} (было prod)\n"),
    );
    // S2: без заголовков
    let base = format!("начало\ntoken: {a} prod eu\nсередина\ntoken: {b}\nконец\n");
    exact(
        &base,
        &format!("начало\nсередина\ntoken: {} prod eu копия\nконец\n", m(2)),
        &format!("начало\nсередина\ntoken: {b} prod eu копия\nконец\n"),
    );
    // V2: длинная строка Dev переписана описанием Prod
    let base = format!(
        "## Prod\n- prod: password = {p1} (основной кластер)\n## Dev\n- dev: password = {p2} (стенд разработки)\n"
    );
    exact(
        &base,
        &format!("## Dev\n- dev: password = {} (основной кластер)\n", m(2)),
        &format!("## Dev\n- dev: password = {p2} (основной кластер)\n"),
    );
    // V3: ключ переименован
    let base = format!("# K\n## Prod\ntoken: {a} (prod)\n## Dev\ndev_token: {b}\n");
    exact(
        &base,
        &format!("# K\n## Dev\ntoken: {} (было prod)\n", m(2)),
        &format!("# K\n## Dev\ntoken: {b} (было prod)\n"),
    );
    // V4: строка Dev стала равна виду строки Prod — номер Dev
    let base = format!("# K\n## Prod\ntoken: {a} (prod)\n## Dev\ntoken: {b}\n");
    exact(
        &base,
        &format!("# K\n## Dev\ntoken: {} (prod)\n", m(2)),
        &format!("# K\n## Dev\ntoken: {b} (prod)\n"),
    );
    // V4′: агент перенёс строку Prod под Dev (номер Prod) — её значение
    exact(
        &base,
        &format!("# K\n## Dev\ntoken: {} (prod)\n", m(1)),
        &format!("# K\n## Dev\ntoken: {a} (prod)\n"),
    );
    // V7: обе правлены на месте, строка Prod — словами строки Dev: без обмена
    let base = format!("# K\n## Prod\ntoken: {a}\n## Dev\ntoken: {b} dev west\n");
    exact(
        &base,
        &format!(
            "# K\n## Prod\ntoken: {} dev west (now prod)\n## Dev\ntoken: {}\n",
            m(1),
            m(2)
        ),
        &format!("# K\n## Prod\ntoken: {a} dev west (now prod)\n## Dev\ntoken: {b}\n"),
    );
    // db1/db3: db1 удалена, db3 правлена и перенесена на её место
    let base = format!(
        "## S\n- db1: password = {p1}\n- db2: password = {p2}\n- db3: password = {p3}\nконец\n"
    );
    exact(
        &base,
        &format!(
            "## S\n- db3: password = {} (новый)\n- db2: password = {}\nконец\n",
            m(3),
            m(2)
        ),
        &format!("## S\n- db3: password = {p3} (новый)\n- db2: password = {p2}\nконец\n"),
    );
    // две метки в строке переставлены
    exact(
        &format!("a: {a} b: {b}\n"),
        &format!("b: {} a: {}\n", m(2), m(1)),
        &format!("b: {b} a: {a}\n"),
    );
}

/// Пересканирование (C1 круга 1): снят забор кода вокруг флага, URL с паролем переписан прозой,
/// у ключа снят разделитель — значение вышло бы из-под маски: строки — в `exposed`; контекст
/// сохранён — `exposed` пуст.
#[test]
fn restore_context_changed_exposed() {
    let p = pw(41);
    let base = format!("# Стенд\n\n```bash\nmysql -p{p} db\n```\n");
    let v = numbered_view(&base).into_owned();
    assert!(v.contains(&format!("mysql -p{} db", m(1))), "{v}");
    let r = restore(&base, &v.replace("```bash\n", "").replace("```\n", ""));
    assert_eq!(r.exposed, vec![3], "{}", r.text);
    let r = restore(&base, &v.replace("db\n", "db --verbose\n"));
    assert!(r.exposed.is_empty() && r.text.contains(&p), "{}", r.text);
    let base = format!("база: https://user:{p}@db.example.com/\n");
    let v = numbered_view(&base).into_owned();
    let r = restore(
        &base,
        &v.replace("https://user:", "логин user, пароль ")
            .replace("@db.example.com/", " (db.example.com)"),
    );
    assert_eq!(r.exposed, vec![1], "{}", r.text);
    let r = restore(&base, &v.replace("база:", "база prod:"));
    assert!(r.exposed.is_empty() && r.text.contains(&p), "{}", r.text);
    let base = format!("x\npassword = {p}\n");
    let v = numbered_view(&base).into_owned();
    assert_eq!(
        restore(&base, &v.replace("password = ", "password ")).exposed,
        vec![2]
    );
    let r = restore(&base, &v.replace("password = ", "password (стенд) = "));
    assert!(r.exposed.is_empty() && r.text.contains(&p), "{}", r.text);
}

/// База с `\r\n`, правка с `\n` — значения на месте, окончания — как у правки; многострочное
/// значение (закрытый ключ) — с окончаниями правки.
#[test]
fn restore_crlf_and_multiline() {
    let k = key(51);
    let base = format!("# K\r\n\r\ntoken: {k}\r\nпрочее\r\n");
    let edited = numbered_view(&base)
        .replace("\r\n", "\n")
        .replace("прочее", "прочее 2");
    exact(&base, &edited, &format!("# K\n\ntoken: {k}\nпрочее 2\n"));
    let pem = format!(
        "-----BEGIN {}PRIVATE KEY-----\r\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASC\r\n-----END {}PRIVATE KEY-----",
        "RSA ", "RSA "
    );
    let base = format!("ключ:\r\n{pem}\r\nконец\r\n");
    let v = numbered_view(&base);
    assert_eq!(v, format!("ключ:\r\n{}\r\nконец\r\n", m(1)));
    exact(
        &base,
        &v.replace("\r\n", "\n").replace("конец", "конец 2"),
        &format!("ключ:\n{}\nконец 2\n", pem.replace("\r\n", "\n")),
    );
}

/// Поле из нескольких строк (элементы массива, теги): нумерация сквозная, элемент перенесён в
/// другое место или в другой элемент — значение по номеру (V2d, I5 круга 2).
#[test]
fn field_masks_items() {
    let (a, b) = (key(81), key(82));
    let (p1, p2) = (pw(81), pw(82));
    let items = [
        format!("prod: password = {p1} (основной кластер)"),
        format!("dev: password = {p2} (стенд)"),
        format!("CI: {a}"),
        format!("Dev: {b}"),
    ];
    let mut f = FieldMasks::new();
    let views: Vec<String> = items.iter().map(|s| f.view(s).into_owned()).collect();
    assert_eq!(views[3], format!("Dev: {}", m(4)));
    // V2d: элемент dev переписан описанием prod (prod удалён)
    let r = f.restore(&format!("dev: password = {} (основной кластер)", m(2)));
    assert_eq!(r.text, format!("dev: password = {p2} (основной кластер)"));
    // I5: элемент CI удалён, Dev получил слово CI
    let r = f.restore(&format!("Dev CI: {}", m(4)));
    assert_eq!(r.text, format!("Dev CI: {b}"));
    // два значения в одном элементе и номер вне поля
    let r = f.restore(&format!("{} и {} и {}", m(3), m(1), m(9)));
    assert_eq!(r.text, format!("{a} и {p1} и {}", m(9)));
    assert_eq!(r.kept, 1);
}

/// Отложенные значения: заменители в кусках, сборка, затем значения и пересканирование; номер
/// строки отказа — строка итога, где значение — одна строка.
#[test]
fn deferred_values() {
    let (k, p) = (key(91), pw(91));
    let base = format!("token: {k}\npassword = {p}\n");
    let f = FieldMasks::of(&base);
    let piece = format!("новое: {}\npassword {}\nx {M}\n", m(1), m(2));
    let mut d = Deferred::new(&[&base, &piece, "хвост\n"]).unwrap();
    let a = f.defer(&piece, &mut d);
    assert!(!a.contains(&k) && !a.contains(&p) && a.contains(M), "{a}");
    assert_eq!(d.kept(), 1);
    let r = d.finish(&format!("# Заголовок\n{a}хвост\n"));
    assert_eq!(
        r.text,
        format!("# Заголовок\nновое: {k}\npassword {p}\nx {M}\nхвост\n")
    );
    assert_eq!((r.kept, r.exposed.clone()), (1, vec![3]));
    // многострочное значение — одна строка в номерах
    let pem = format!(
        "-----BEGIN {}PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASC\n-----END {}PRIVATE KEY-----",
        "RSA ", "RSA "
    );
    let base = format!("{pem}\npassword = {p}\n");
    let f = FieldMasks::of(&base);
    let piece = format!("{}\npassword {}\n", m(1), m(2));
    let mut d = Deferred::new(&[&base, &piece]).unwrap();
    let a = f.defer(&piece, &mut d);
    let r = d.finish(&a);
    assert_eq!(r.exposed, vec![2], "{}", r.text);
    // адрес секции — к заголовку исходника
    let base = format!("## Стенд {k}\n");
    let f = FieldMasks::of(&base);
    assert_eq!(
        f.fill(&format!("K > Стенд {}", m(1))),
        format!("K > Стенд {k}")
    );
    assert_eq!(f.fill(&format!("Стенд {M}")), format!("Стенд {M}"));
}

/// Псевдослучайный генератор (LCG) — воспроизводимые правки.
struct Rng(u64);

impl Rng {
    fn next(&mut self, n: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % n.max(1)
    }
}

/// Строка `i` исходника: проза, ключ, пароль ключ-значения, пароль в URL, два секрета или
/// однообразная строка без номера.
fn line(i: u32, kind: usize) -> String {
    match kind {
        0 => format!("строка {i}: обычный текст про сборку\n"),
        1 => format!("ключ {i}: {}\n", key(i)),
        2 => format!("строка {i}: password = {}\n", pw(i)),
        3 => format!("строка {i}: https://user:{}@db.example.com/x\n", pw(i)),
        4 => format!("строка {i}: {} и password = {}\n", key(i), pw(i)),
        _ => format!("token: {}\n", key(i)),
    }
}

/// Правка строки вокруг меток: суффикс, префикс или другое слово в начале.
fn edit_line(s: &mut String, e: usize) {
    let end = s.trim_end_matches(['\r', '\n']).len();
    match e % 3 {
        0 => s.insert_str(end, &format!(" (правка {e})")),
        1 => s.insert_str(0, "- "),
        _ => {
            if let Some(rest) = s.strip_prefix("строка ") {
                *s = format!("запись {rest}");
            } else {
                s.insert_str(0, "> ");
            }
        }
    }
}

/// Случайные правки агента по нумерованному виду: перестановка, удаление, дублирование строк с
/// метками, вставка, правка слов вокруг меток; база с `\r\n` и правка с `\n`. Итог — ровно те же
/// правки исходника (значения по номерам, повтор — тем же значением), меток без пары и отказов
/// нет. Зерно и число случаев — `T6_SEED`, `T6_CASES`.
#[test]
fn restore_random_line_edits() {
    let env = |k: &str| std::env::var(k).ok().and_then(|s| s.parse::<u64>().ok());
    let mut rng = Rng(env("T6_SEED").unwrap_or(7));
    for case in 0..env("T6_CASES").unwrap_or(1000) as u32 {
        let crlf = rng.next(4) == 0;
        let mut raw: Vec<String> = Vec::new();
        for s in 0..1 + rng.next(5) {
            raw.push(format!("## Секция {case}-{s}\n"));
            for _ in 0..1 + rng.next(8) {
                let i = case * 1000 + raw.len() as u32;
                raw.push(line(i, rng.next(6)));
            }
        }
        if crlf {
            for l in &mut raw {
                *l = l.replace('\n', "\r\n");
            }
        }
        let base: String = raw.concat();
        let view = numbered_view(&base).into_owned();
        let mut seen: Vec<String> = view.split_inclusive('\n').map(str::to_owned).collect();
        assert_eq!(raw.len(), seen.len(), "строки вида — как строки исходника");
        if crlf {
            for l in raw.iter_mut().chain(seen.iter_mut()) {
                *l = l.replace("\r\n", "\n");
            }
        }
        for e in 0..1 + rng.next(9) {
            let len = raw.len();
            match rng.next(6) {
                0 => {
                    let at = rng.next(len + 1);
                    let t = format!("вставка {case}-{e}\n");
                    raw.insert(at, t.clone());
                    seen.insert(at, t);
                }
                1 if len > 1 => {
                    let at = rng.next(len);
                    raw.remove(at);
                    seen.remove(at);
                }
                2 => {
                    let at = rng.next(len);
                    edit_line(&mut raw[at], e);
                    edit_line(&mut seen[at], e);
                }
                3 => {
                    // дубль строки (с её метками) в другом месте
                    let (from, to) = (rng.next(len), rng.next(len + 1));
                    let (r, s) = (raw[from].clone(), seen[from].clone());
                    raw.insert(to, r);
                    seen.insert(to, s);
                }
                _ => {
                    let from = rng.next(len);
                    let (r, s) = (raw.remove(from), seen.remove(from));
                    let to = rng.next(len);
                    raw.insert(to, r);
                    seen.insert(to, s);
                }
            }
        }
        let (want, edited) = (raw.concat(), seen.concat());
        let r = restore(&base, &edited);
        assert_eq!(r.text, want, "случай {case}:\n{edited}");
        assert_eq!(
            (r.kept, r.exposed.len()),
            (0, 0),
            "случай {case}:\n{edited}"
        );
    }
}

/// Цена (журнал этапа): заметка 1 МБ и 5 МиБ — разделы с прозой, кодом и ключом на ~157 байт;
/// нумерованный вид и восстановление правки (дописка к десяти строкам с метками, вставка и
/// удаление строк; худший случай — правлена каждая строка). Медиана 5 прогонов, только release:
/// `cargo test --release -p zam-secrets --features restore --lib -- --ignored bench_restore
/// --nocapture`.
#[test]
#[ignore]
fn bench_restore() {
    let doc = |size: usize| {
        let k = key(1);
        let mut t = String::from("# Заметка о стенде\n\n");
        let mut i = 0usize;
        while t.len() < size {
            t.push_str(&format!(
                "## Раздел {i}\n\nСборка идёт скриптом `deploy.sh`, журнал — в \
                 `/var/log/app`.\nThe service reads its config at start.\n\n```bash\ncurl -s \
                 https://api.example.com/v1/items?page={i}\n```\n\ntoken: {k}\n\nСсылка: \
                 https://user:pw{i}x@db.example.com/\n\n"
            ));
            i += 1;
        }
        t
    };
    let median = |f: &mut dyn FnMut()| {
        let mut times = Vec::new();
        for _ in 0..5 {
            let t0 = std::time::Instant::now();
            f();
            times.push(t0.elapsed());
        }
        times.sort();
        times[2]
    };
    for size in [1024 * 1024, 5 * 1024 * 1024] {
        let base = doc(size);
        let view = numbered_view(&base).into_owned();
        let t_view = median(&mut || {
            std::hint::black_box(numbered_view(&base));
        });
        let mut lines: Vec<String> = view.split_inclusive('\n').map(str::to_owned).collect();
        let step = lines.len() / 10;
        for j in 0..10 {
            let at = j * step + 1;
            lines.insert(at, format!("вставка {j}\n"));
            lines.remove(at + 3);
        }
        let mut edited_marks = 0;
        for l in lines.iter_mut() {
            if l.starts_with("token: ") && edited_marks < 10 {
                l.insert_str(l.len() - 1, " (prod)");
                edited_marks += 1;
            }
        }
        let small: String = lines.concat();
        let all: String = view
            .split_inclusive('\n')
            .map(|l| format!("> {l}"))
            .collect();
        for (what, edited) in [("правка", &small), ("каждая строка", &all)] {
            let mut kept = 0;
            let t = median(&mut || kept = restore(&base, edited).kept);
            println!(
                "{} байт: вид {t_view:?}; восстановление ({what}, {} меток, без пары {kept}) {t:?}",
                base.len(),
                marks_in(edited),
            );
        }
    }
}
