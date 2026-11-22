use super::*;
use crate::mask_text;

/// Ключ из частей (в исходнике целиком его нет — push protection); `n` — отличие ключей.
fn key(n: u32) -> String {
    format!("gh{}_{}{n:04}", "p", "aB3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1eF")
}

/// URL с паролем из частей: в исходнике нет пары `логин:пароль@` (фиктивные секреты — склейкой).
fn url(head: &str, pass: &str, tail: &str) -> String {
    format!("{head}:{pass}@{tail}")
}

/// Пароль ключ-значения из частей.
fn pw(n: u32) -> String {
    format!("{}{n}{}", "hunter", "Qz9x")
}

const M: &str = SECRET_MARK;
/// Метка объекта и ревизии в тестах.
const T: &str = "0a1b2c";

/// Метка объекта `T` с номером `n`.
fn m(n: u32) -> String {
    format!("‹secret-{T}-{n}›")
}

/// Все метки — `‹secret›`.
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

/// Вид объекта из одного текста.
fn view(raw: &str) -> String {
    numbered_view(T, raw).into_owned()
}

/// Восстановление по меткам: ожидаемый текст, без предупреждения и отказа.
#[track_caller]
fn exact(base: &str, edited: &str, want: &str) {
    let r = restore(T, base, edited);
    assert_eq!(r.text, want, "правка {edited:?}");
    assert_eq!((r.kept, r.exposed.len()), (0, 0), "правка {edited:?}");
}

/// Отказ (значение вне покрытия) на строках `lines`.
#[track_caller]
fn exposed(base: &str, edited: &str, lines: &[usize]) {
    let r = restore(T, base, edited);
    assert_eq!(r.exposed, lines, "правка {edited:?} → {:?}", r.text);
}

/// Вид: маски — `‹secret-T-1›`, `‹secret-T-2›`, …; без меток вид — ровно `mask_text`; литералы
/// меток исходника (`‹secret›`, `‹secret-N›`, `‹secret-T-N›`) — `‹secret›`; нумерация объекта —
/// сквозная по полям.
#[test]
fn numbered_view_order() {
    let (k1, k2) = (key(1), key(2));
    let base = format!(
        "# K\ntoken: {k1}\nbase: https://user:{}@db.example.com/\nb: {k2}\n",
        pw(1)
    );
    let v = view(&base);
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
    assert!(matches!(numbered_view(T, "без секретов"), Cow::Borrowed(_)));
    let lit = format!("было {M} и ‹secret-7› и ‹secret-ffffff-2› и token: {k1}\n");
    assert_eq!(
        view(&lit),
        format!("было {M} и {M} и {M} и token: {}\n", m(1))
    );
    assert_eq!(literals_in(&lit), 3);
    // объект из нескольких полей — сквозная нумерация
    let mut o = ObjectMasks::new(T);
    let (a, b) = (format!("a {k1}"), format!("b {k2}"));
    assert_eq!(o.view(&a), format!("a {}", m(1)));
    assert_eq!(o.view("c"), "c");
    assert_eq!(o.view(&b), format!("b {}", m(2)));
    assert_eq!(o.len(), 2);
    assert_eq!(o.value(Mark::Tagged(T, 2)), Some(k2.as_str()));
    assert_eq!(o.value(Mark::Tagged(T, 3)), None);
    assert_eq!(o.value(Mark::Tagged("ffffff", 1)), None);
    assert_eq!(o.value(Mark::Numbered(1)), None);
    assert_eq!(unnumbered(&v), mask_text(&base));
}

/// Вид без правки — исходник; правка вокруг метки, перестановка, перенос, удаление, повтор — по
/// меткам.
#[test]
fn restore_by_mark() {
    let (k1, k2) = (key(11), key(12));
    let base = format!("# K\n\na: {k1}\nb: {k2}\n\nхвост\n");
    exact(&base, &view(&base), &base);
    exact(
        &base,
        &format!("# K\n\nb: {} (prod)\nтекст\na: {}\n", m(2), m(1)),
        &format!("# K\n\nb: {k2} (prod)\nтекст\na: {k1}\n"),
    );
    exact(&base, "# K\n", "# K\n");
    exact(
        &base,
        &format!("a: {}\nкопия: {}\n", m(1), m(1)),
        &format!("a: {k1}\nкопия: {k1}\n"),
    );
}

/// Литералы (§4.3.2 п. 2): чужая метка объекта, номер вне базы, `‹secret›`, прежняя `‹secret-N›` —
/// как есть; предупреждение — литералов итога сверх литералов исходника поля.
#[test]
fn restore_literals_and_kept() {
    let k = key(21);
    let base = format!("token: {k}\n");
    let r = restore(
        T,
        &base,
        &format!(
            "token: {}\nx {M}\ny {}\nz ‹secret-1›\nw ‹secret-ffffff-1›\n",
            m(1),
            m(2)
        ),
    );
    assert_eq!(
        r.text,
        format!(
            "token: {k}\nx {M}\ny {}\nz ‹secret-1›\nw ‹secret-ffffff-1›\n",
            m(2)
        )
    );
    assert_eq!((r.kept, r.exposed.len()), (4, 0));
    // литерал архива напротив себя — не предупреждение; дописанный — предупреждение (M-c)
    let lit = format!("сервис: {M}\nещё ‹secret-3›\n");
    let r = restore(T, &lit, &view(&lit));
    assert_eq!(
        (r.text.as_str(), r.kept),
        (format!("сервис: {M}\nещё {M}\n").as_str(), 0)
    );
    let r = restore(T, &lit, &format!("{}новый {M}\n", view(&lit)));
    assert_eq!(r.kept, 1);
    assert_eq!(restore(T, &base, "без меток").text, "без меток");
    assert!(has_mark(&m(3)) && has_mark(M) && !has_mark("‹secret-›") && !has_mark("secret"));
    assert_eq!(marks_in(&format!("{M}{}{M} ‹secret-12›", m(1))), 4);
}

/// Пробы ревью кругов 1–4 (S1, S2, V1–V7, db1/db3): значение — по метке, которую агент оставил у
/// строки.
#[test]
fn restore_review_cases() {
    let (a, b) = (key(1), key(2));
    let (p1, p2, p3) = (pw(1), pw(2), pw(3));
    let base = format!("# K\n## Prod\ntoken: {a} (prod)\n## Dev\ntoken: {b}\n");
    exact(
        &base,
        &format!("# K\n## Dev\ntoken: {} (было prod)\n", m(2)),
        &format!("# K\n## Dev\ntoken: {b} (было prod)\n"),
    );
    exact(
        &base,
        &format!("# K\n## Dev\ntoken: {} (prod)\n", m(1)),
        &format!("# K\n## Dev\ntoken: {a} (prod)\n"),
    );
    let base = format!("начало\ntoken: {a} prod eu\nсередина\ntoken: {b}\nконец\n");
    exact(
        &base,
        &format!("начало\nсередина\ntoken: {} prod eu копия\nконец\n", m(2)),
        &format!("начало\nсередина\ntoken: {b} prod eu копия\nконец\n"),
    );
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
}

/// Покрытие по происхождению (§4.3.2 п. 5): снят забор, URL прозой, снят разделитель — отказ на
/// строке итога; контекст сохранён — нет.
#[test]
fn restore_context_changed_exposed() {
    let p = pw(41);
    let base = format!("# Стенд\n\n```bash\nmysql -p{p} db\n```\n");
    let v = view(&base);
    assert!(v.contains(&format!("mysql -p{} db", m(1))), "{v}");
    exposed(
        &base,
        &v.replace("```bash\n", "").replace("```\n", ""),
        &[3],
    );
    exposed(&base, &v.replace("db\n", "db --verbose\n"), &[]);
    let base = format!("база: https://user:{p}@db.example.com/\n");
    let v = view(&base);
    exposed(
        &base,
        &v.replace("https://user:", "логин user, пароль ")
            .replace("@db.example.com/", " (db.example.com)"),
        &[1],
    );
    exposed(&base, &v.replace("база:", "база prod:"), &[]);
    let base = format!("x\npassword = {p}\n");
    let v = view(&base);
    exposed(&base, &v.replace("password = ", "password "), &[2]);
    exposed(&base, &v.replace("password = ", "password (стенд) = "), &[]);
}

/// Слабые значения (круг правок 5, I-A): пароль URL, совпадающий с логином и схемой, и пароль из
/// одного знака, переписанные прозой, — отказ; обычные слова рядом со слабым значением (I-B) и
/// дописанные догадки (I-C) — не отказ.
#[test]
fn weak_values() {
    let pg = &format!(
        "БД: {}\n",
        url("postgres://postgres", "postgres", "localhost:5432/app")
    );
    let v = view(pg);
    assert_eq!(
        v,
        format!(
            "БД: {}\n",
            url("postgres://postgres", &m(1), "localhost:5432/app")
        )
    );
    exposed(
        pg,
        &format!("БД: логин postgres, пароль {}, хост localhost\n", m(1)),
        &[1],
    );
    exposed(pg, &format!("БД: пароль {}\n", m(1)), &[1]);
    let mq = &format!(
        "Очередь: {}\n",
        url("amqp://guest", "guest", "localhost:5672/")
    );
    exposed(mq, &format!("Очередь: пароль {}\n", m(1)), &[1]);
    let one = &format!("Строка {}\n", url("postgres://u", "p", "h/db"));
    assert!(view(one).contains(&m(1)), "{}", view(one));
    exposed(one, &format!("Строка: пароль {}\n", m(1)), &[1]);
    // I-B: набранный агентом текст не проверяется
    let rd = &format!("Кэш: {}\n", url("redis://", "pass", "localhost:6379"));
    exposed(
        rd,
        &format!("{}Сменить password раз в квартал\n", view(rd)),
        &[],
    );
    let pp = &format!(
        "POSTGRES_PASSWORD: {}\nвход: {}\n",
        ["pass", "word"].concat(),
        url("redis://", "pass", "h")
    );
    exposed(pp, &format!("{}password policy: 90 дней\n", view(pp)), &[]);
    exposed(
        pg,
        &format!("{}Перезапустить postgres после обновления\n", view(pg)),
        &[],
    );
    exposed(one, &format!("{}deploy\n", view(one)), &[]);
    let line = view(pg);
    exposed(pg, &format!("{line}{line}"), &[]);
    // I-C: догадки без меток — без отказа (строки ничего не называют)
    let base = format!("# Стенд\n\npassword = {}\n", pw(31));
    let guesses: String = (20..40).map(|k| format!("{}\n", pw(k))).collect();
    exposed(&base, &format!("{}{guesses}", view(&base)), &[]);
}

/// Метки с личностью (круг правок 5, I-D): перенос метки из поля в поле объекта — значение; копия
/// из другого объекта (чужой T) и прежняя `‹secret-N›` без T — литерал.
#[test]
fn object_marks_across_fields() {
    let (k, p) = (key(81), pw(81));
    let (title, body) = (format!("Ключ CI {k}"), format!("Стенд\npassword = {p}\n"));
    let mut o = ObjectMasks::new(T);
    let tv = o.view(&title).into_owned();
    let bv = o.view(&body).into_owned();
    assert_eq!(
        (tv.as_str(), bv.as_str()),
        (
            format!("Ключ CI {}", m(1)).as_str(),
            format!("Стенд\npassword = {}\n", m(2)).as_str()
        )
    );
    // строка пароля перенесена из body в title
    let r = o.restore(&format!("{tv}; password = {}", m(2)), 0);
    assert_eq!(r.text, format!("{title}; password = {p}"));
    assert_eq!((r.kept, r.exposed.len()), (0, 0));
    // копия строки из другой заметки (её T) — литерал и предупреждение
    let other = numbered_view("ffffff", &format!("token: {}\n", key(82))).into_owned();
    let r = o.restore(&format!("{bv}{other}"), 0);
    assert_eq!(r.text, format!("{body}{other}"));
    assert_eq!(r.kept, 1);
    // вид круга 3 (`‹secret-1›` без T) — литерал
    let r = restore(T, &body, "password = ‹secret-1›\n");
    assert_eq!((r.text.as_str(), r.kept), ("password = ‹secret-1›\n", 1));
}

/// База с `\r\n`, правка с `\n`; многострочное значение — с окончаниями правки; строка отказа —
/// строка итога с переводами строк многострочного значения.
#[test]
fn restore_crlf_and_multiline() {
    let k = key(51);
    let base = format!("# K\r\n\r\ntoken: {k}\r\nпрочее\r\n");
    let edited = view(&base)
        .replace("\r\n", "\n")
        .replace("прочее", "прочее 2");
    exact(&base, &edited, &format!("# K\n\ntoken: {k}\nпрочее 2\n"));
    let pem = format!(
        "-----BEGIN {}PRIVATE KEY-----\r\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASC\r\n-----END {}PRIVATE KEY-----",
        "RSA ", "RSA "
    );
    let base = format!("ключ:\r\n{pem}\r\npassword = {}\r\nконец\r\n", pw(52));
    let v = view(&base);
    assert_eq!(
        v,
        format!("ключ:\r\n{}\r\npassword = {}\r\nконец\r\n", m(1), m(2))
    );
    exact(
        &base,
        &v.replace("\r\n", "\n").replace("конец", "конец 2"),
        &format!(
            "ключ:\n{}\npassword = {}\nконец 2\n",
            pem.replace("\r\n", "\n"),
            pw(52)
        ),
    );
    exposed(
        &base,
        &v.replace("\r\n", "\n").replace("password = ", "password "),
        &[5],
    );
}

/// Отложенные значения (`PATCH`): незакрытый забор над блоком с `-p…` открывает маску текущей
/// ревизии, перенесённую без правки (C1); значения из меток операции и перенесённые — позиции в
/// итоге; адрес секции — к заголовку исходника.
#[test]
fn deferred_values() {
    let p = pw(95);
    let current = format!("# K\n\n```\nmysql -uroot -p{p} db\n```\n\nконец\n");
    let o = {
        let mut o = ObjectMasks::new(T);
        o.add(&current);
        o
    };
    let keep = secret_ranges(&current);
    assert_eq!(keep.len(), 1);
    // prepend «```\n»: перенесённая маска сдвигается на 4 байта
    let mut d = Deferred::new(&[&current, "```\n"]).unwrap();
    let piece = o.defer("```\n", &mut d);
    let shifted: Vec<Range<usize>> = keep.iter().map(|r| r.start + 4..r.end + 4).collect();
    let f = d.finish(&format!("{piece}{current}"), &shifted);
    assert_eq!(f.text, format!("```\n{current}"));
    assert_eq!(f.at, shifted);
    assert_eq!(uncovered_lines(&f.text, &f.at), vec![5]);
    // метки операции: значение в коде — покрыто, прозой — нет
    let piece = format!("```\nmysql -p{} x\n```\nпароль {}\n", m(1), m(1));
    let mut d = Deferred::new(&[&current, &piece]).unwrap();
    let a = o.defer(&piece, &mut d);
    assert!(!a.contains(&p), "{a}");
    let text = format!("{current}{a}");
    let f = d.finish(&text, &keep);
    assert_eq!(
        f.text,
        format!("{current}```\nmysql -p{p} x\n```\nпароль {p}\n")
    );
    assert_eq!(f.at.len(), 3);
    assert_eq!(uncovered_lines(&f.text, &f.at), vec![11]);
    assert_eq!(f.literals, 0);
    // адрес секции
    let head = format!("## Стенд {}\n", key(96));
    let mut o = ObjectMasks::new(T);
    o.add(&head);
    assert_eq!(
        o.fill(&format!("K > Стенд {}", m(1))),
        format!("K > Стенд {}", key(96))
    );
    assert_eq!(o.fill(&format!("Стенд {M}")), format!("Стенд {M}"));
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

/// Строка `i` исходника: проза, ключ, пароль ключ-значения, пароль в URL, два секрета, однообразная
/// строка и слабые значения (совпадают с логином и схемой, из одного знака, подстрока ключа).
fn line(i: u32, kind: usize) -> String {
    match kind {
        0 => format!("строка {i}: обычный текст про сборку\n"),
        1 => format!("ключ {i}: {}\n", key(i)),
        2 => format!("строка {i}: password = {}\n", pw(i)),
        3 => format!("строка {i}: https://user:{}@db.example.com/x\n", pw(i)),
        4 => format!("строка {i}: {} и password = {}\n", key(i), pw(i)),
        5 => format!("token: {}\n", key(i)),
        6 => format!(
            "строка {i}: {}\n",
            url("postgres://postgres", "postgres", &format!("db{i}/app"))
        ),
        7 => format!(
            "строка {i}: {}\n",
            url("amqp://guest", "guest", &format!("mq{i}:5672/"))
        ),
        8 => format!(
            "строка {i}: {}\n",
            url("redis://", "p", &format!("cache{i}:6379"))
        ),
        _ => format!("строка {i}: password = {}\n", &key(i)[4..14]),
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

/// Слова набранного агентом текста — со слабыми значениями (`password`, `postgres`, `guest`, `p`).
const PROSE: &[&str] = &[
    "сменить password раз в квартал\n",
    "перезапустить postgres\n",
    "пользователь guest\n",
    "p\n",
    "deploy\n",
];

/// Случайные правки агента по виду: перестановка, удаление, дублирование строк с метками, вставка
/// прозы со словами слабых значений, правка слов вокруг меток; база с `\r\n` и правка с `\n`; слабые
/// значения. Итог — ровно те же правки исходника (значения по меткам, повтор — тем же значением),
/// предупреждений и отказов нет. Зерно и число случаев — `T6_SEED`, `T6_CASES`.
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
                raw.push(line(i, rng.next(10)));
            }
        }
        if crlf {
            for l in &mut raw {
                *l = l.replace('\n', "\r\n");
            }
        }
        let base: String = raw.concat();
        let v = view(&base);
        let mut seen: Vec<String> = v.split_inclusive('\n').map(str::to_owned).collect();
        assert_eq!(raw.len(), seen.len(), "строки вида — как строки исходника");
        if crlf {
            for l in raw.iter_mut().chain(seen.iter_mut()) {
                *l = l.replace("\r\n", "\n");
            }
        }
        for e in 0..1 + rng.next(9) {
            let len = raw.len();
            match rng.next(7) {
                0 => {
                    let at = rng.next(len + 1);
                    let t = PROSE[rng.next(PROSE.len())].to_string();
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
        let r = restore(T, &base, &edited);
        assert_eq!(r.text, want, "случай {case}:\n{edited}");
        assert_eq!(
            (r.kept, r.exposed.len()),
            (0, 0),
            "случай {case}:\n{edited}"
        );
    }
}

/// Цена (журнал этапа): заметка 1 МБ и 5 МиБ — разделы с прозой, кодом и ключом на ~157 байт; вид,
/// восстановление правки (дописка к десяти строкам с метками, вставка и удаление строк; худший
/// случай — правлена каждая строка) и `PATCH`, где содержимое операции — вид всей заметки (тысячи
/// масок). Медиана 5 прогонов, только release: `cargo test --release -p zam-secrets --features
/// restore --lib -- --ignored bench_restore --nocapture`.
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
    for size in [1024 * 1024, 5 * 1024 * 1024 / 2, 5 * 1024 * 1024] {
        let base = doc(size);
        let v = view(&base);
        let t_view = median(&mut || {
            std::hint::black_box(numbered_view(T, &base));
        });
        let mut lines: Vec<String> = v.split_inclusive('\n').map(str::to_owned).collect();
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
        let all: String = v.split_inclusive('\n').map(|l| format!("> {l}")).collect();
        for (what, edited) in [("правка", &small), ("каждая строка", &all)] {
            let mut kept = 0;
            let t = median(&mut || kept = restore(T, &base, edited).kept);
            println!(
                "{} байт: вид {t_view:?}; восстановление ({what}, {} меток, литералов {kept}) {t:?}",
                base.len(),
                marks_in(edited),
            );
        }
        // как у сервера: маски текущей ревизии (`secret_ranges`) — до операций; вид всей заметки
        // заменяет текст целиком (перенесённых нет)
        let mut exp = 0;
        let t = median(&mut || {
            let mut o = ObjectMasks::new(T);
            o.add(&base);
            std::hint::black_box(secret_ranges(&base));
            let mut d = Deferred::new(&[&base, &v]).unwrap();
            let content = o.defer(&v, &mut d);
            let f = d.finish(&content, &[]);
            assert_eq!(f.text, base);
            exp = uncovered_lines(&f.text, &f.at).len();
        });
        println!(
            "{} байт: PATCH видом всей заметки ({} меток, отказов {exp}) {t:?}",
            base.len(),
            marks_in(&v),
        );
        // дописка строки в конец: все маски текущей ревизии перенесены и проверяются в итоге
        let t = median(&mut || {
            let mut o = ObjectMasks::new(T);
            o.add(&base);
            let keep = secret_ranges(&base);
            let line = "новая строка\n";
            let mut d = Deferred::new(&[&base, line]).unwrap();
            let text = format!("{base}{}", o.defer(line, &mut d));
            let f = d.finish(&text, &keep);
            exp = uncovered_lines(&f.text, &f.at).len();
        });
        println!(
            "{} байт: PATCH дописка строки ({} перенесённых масок, отказов {exp}) {t:?}",
            base.len(),
            secret_ranges(&base).len(),
        );
    }
}
