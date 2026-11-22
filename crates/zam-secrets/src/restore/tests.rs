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

/// Метки, оставшиеся напротив метки вида, получают значение; дописанное — как есть.
#[test]
fn restore_unchanged_marks() {
    let k = key(1);
    let base = format!("a token: {k} b");
    assert_eq!(mask_text(&base), format!("a token: {M} b"));
    let r = restore(&base, &format!("a token: {M} b c"));
    assert_eq!(r.text, format!("a token: {k} b c"));
    assert_eq!((r.kept, r.exposed.len()), (0, 0));
}

/// Строка с меткой правлена вокруг метки — значение на месте; соседние строки — как есть.
#[test]
fn restore_line_edited_around_mark() {
    let k = key(2);
    let base = format!("# Ключи\n\ntoken: {k}\n\nконец\n");
    let r = restore(&base, &format!("# Ключи\n\ntoken: {M} (prod)\n\nконец\n"));
    assert_eq!(r.text, format!("# Ключи\n\ntoken: {k} (prod)\n\nконец\n"));
    assert_eq!(r.kept, 0);
    let r = restore(
        &base,
        &format!("# Ключи\n\nboevoy token: {M}, не трогать\n\nконец\n"),
    );
    assert_eq!(
        r.text,
        format!("# Ключи\n\nboevoy token: {k}, не трогать\n\nконец\n")
    );
    assert_eq!(r.kept, 0);
}

/// Две строки с ключами переставлены — каждый ключ остаётся при своей строке; перенос через
/// неизменённые строки.
#[test]
fn restore_two_secrets_order() {
    let (k1, k2) = (key(11), key(12));
    let base = format!("# K\n\na: {k1}\nb: {k2}\n\nхвост\n");
    let r = restore(&base, &format!("# K\n\nb: {M}\na: {M}\n\nхвост\n"));
    assert_eq!(r.text, format!("# K\n\nb: {k2}\na: {k1}\n\nхвост\n"));
    assert_eq!(r.kept, 0);
    let base = format!("a: {k1}\nx\ny\nz\nb: {k2}\n");
    let r = restore(&base, &format!("x\nb: {M}\ny\nz\na: {M}\n"));
    assert_eq!(r.text, format!("x\nb: {k2}\ny\nz\na: {k1}\n"));
    assert_eq!(r.kept, 0);
}

/// Новая строка с меткой — литерал и `kept`; прежняя — значение.
#[test]
fn restore_inserted_mark_kept() {
    let k = key(3);
    let base = format!("token: {k}\nконец\n");
    let r = restore(&base, &format!("token: {M}\nx {M}\nконец\n"));
    assert_eq!(r.text, format!("token: {k}\nx {M}\nконец\n"));
    assert_eq!(r.kept, 1);
    let r = restore(&base, &format!("token: {M} и ещё {M}\nконец\n"));
    assert_eq!(r.text, format!("token: {k} и ещё {M}\nконец\n"));
    assert_eq!(r.kept, 1);
}

/// Строка с ключом удалена — ключа нет, `kept == 0`.
#[test]
fn restore_deleted_secret() {
    let (k1, k2) = (key(4), key(5));
    let base = format!("a\ntoken: {k1}\nb\nkey2: {k2}\n");
    let r = restore(&base, &format!("a\nb\nkey2: {M}\n"));
    assert_eq!(r.text, format!("a\nb\nkey2: {k2}\n"));
    assert_eq!(r.kept, 0);
    let r = restore(&base, "a\nb\n");
    assert_eq!((r.text.as_str(), r.kept), ("a\nb\n", 0));
}

/// Быстрые пути: вид без правки — исходник; без меток — как есть; база без секретов — все
/// метки как есть.
#[test]
fn restore_fast_paths() {
    let k = key(6);
    let base = format!("# K\n\ntoken: {k}\npassword = {}\n", pw(6));
    assert_eq!(restore(&base, &mask_text(&base)).text, base);
    assert_eq!(restore(&base, "новое без меток").text, "новое без меток");
    let r = restore("чистый текст\n", &format!("чистый текст {M}\n"));
    assert_eq!((r.text, r.kept), (format!("чистый текст {M}\n"), 1));
    assert!(has_mark(M) && !has_mark("secret"));
    assert_eq!(marks_in(&format!("{M}{M} {M}")), 3);
}

/// Литерал `‹secret›` базы — метка со значением «сама метка»: напротив себя — не новая.
#[test]
fn restore_literal_mark_in_base() {
    let k = key(7);
    let base = format!("было {M} и token: {k}\nконец\n");
    let r = restore(
        &base,
        &format!("было {M} и token: {M} (prod)\nконец\nещё\n"),
    );
    assert_eq!(
        r.text,
        format!("было {M} и token: {k} (prod)\nконец\nещё\n")
    );
    assert_eq!(r.kept, 0);
}

/// Вид [`MaskedBase`] — ровно `mask_text`, метки вида — все `‹secret›` в нём.
#[test]
fn masked_base_is_mask_text() {
    let k = key(8);
    for raw in [
        String::new(),
        "без секретов\n".into(),
        format!("token: {k}\n"),
        format!("{M}{k} {M}\nhttps://u:{}@h.example.com/\n", pw(1)),
        format!("```bash\nmysql -p{} db\n```\nтекст {M}", pw(2)),
        format!("‹secret{k} secret›"),
        format!("token: {k}\r\nconst\r\n"),
    ] {
        let b = MaskedBase::new(&raw);
        assert_eq!(b.masked(), mask_text(&raw), "{raw}");
        let at: Vec<usize> = b.masked.match_indices(M).map(|(i, _)| i).collect();
        let ours: Vec<usize> = b.marks.iter().map(|m| m.0).collect();
        assert_eq!(at, ours, "{raw}");
    }
}

/// Пустая база, пустая правка, текст без перевода строки в конце, метка в начале и в конце.
#[test]
fn restore_edges() {
    let k = key(9);
    assert_eq!(restore("", "").text, "");
    assert_eq!(restore("", M).kept, 1);
    let base = format!("{k} середина {k}");
    let r = restore(&base, &format!("{M} середина {M}!"));
    assert_eq!(r.text, format!("{k} середина {k}!"));
    let r = restore(&base, "");
    assert_eq!((r.text.as_str(), r.kept), ("", 0));
}

/// Проба ревью 1 и 1b (I1): архив и секция Prod удалены, строка Dev правлена — под `## Dev` ключ
/// Dev (строка того же изменённого участка), ключ Prod выпал.
#[test]
fn restore_section_deleted_same_wording() {
    let (k1, k2) = (key(21), key(22));
    let base = format!(
        "# Ключи\nархив 1\nархив 2\nархив 3\nархив 4\n## Prod\ntoken: {k1}\n## Dev\ntoken: {k2}\n"
    );
    let r = restore(&base, &format!("# Ключи\n## Dev\ntoken: {M} (dev)\n"));
    assert_eq!(r.text, format!("# Ключи\n## Dev\ntoken: {k2} (dev)\n"));
    assert_eq!((r.kept, r.exposed.len()), (0, 0));
    let base = format!(
        "# Стенды\n\nАрхив старых стендов:\n- s1\n- s2\n- s3\n- s4\n- s5\n\n## Prod\n\ntoken: \
         {k1}\n\n## Dev\n\ntoken: {k2}\n"
    );
    let r = restore(&base, &format!("# Стенды\n\n## Dev\n\ntoken: {M} (dev)\n"));
    assert_eq!(r.text, format!("# Стенды\n\n## Dev\n\ntoken: {k2} (dev)\n"));
    assert_eq!(r.kept, 0);
}

/// Однообразные строки в разных секциях: правлена строка каждой второй секции, удалены строки
/// других, — каждая правленая строка получает ключ своей секции.
#[test]
fn restore_same_wording_in_sections() {
    let n = 40;
    let mut base = String::from("# Ключи\n");
    let mut edited = String::from("# Ключи\n");
    let mut want = String::from("# Ключи\n");
    for i in 0..n {
        base.push_str(&format!("## Стенд {i}\n\ntoken: {}\n\n", key(100 + i)));
        edited.push_str(&format!("## Стенд {i}\n\n"));
        want.push_str(&format!("## Стенд {i}\n\n"));
        match i % 3 {
            0 => {
                edited.push_str(&format!("token: {M} (стенд {i})\n\n"));
                want.push_str(&format!("token: {} (стенд {i})\n\n", key(100 + i)));
            }
            1 => {}
            _ => {
                edited.push_str(&format!("> token: {M}\n\n"));
                want.push_str(&format!("> token: {}\n\n", key(100 + i)));
            }
        }
    }
    let r = restore(&base, &edited);
    assert_eq!(r.kept, 0);
    assert!(r.text == want, "ключи не своих секций:\n{}", r.text);
}

/// Проба ревью 5 (M1): строка заменена на месте строкой с другим ключом — значение не
/// переносится, метка — литерал и `kept`; тот же ключ в другом регистре — пара.
#[test]
fn restore_other_key_kept() {
    let k = key(31);
    let base = format!("GITHUB_TOKEN: {k}\nконец\n");
    let r = restore(&base, &format!("OPENAI_KEY: {M}\nконец\n"));
    assert_eq!(r.text, format!("OPENAI_KEY: {M}\nконец\n"));
    assert_eq!(r.kept, 1);
    let r = restore(&base, &format!("github_token: {M} # prod\nконец\n"));
    assert_eq!(r.text, format!("github_token: {k} # prod\nконец\n"));
    assert_eq!(r.kept, 0);
}

/// Пробы ревью 2–4 (C1): смена окружения метки. Снятый забор кода и URL или ключ-значение без
/// разделителя (слово перед меткой то же) — значение восстановлено, но сканер его в итоге не
/// закрывает: `exposed` — строки нового текста. Слово перед меткой сменилось (URL переписан
/// прозой, «пароль от стенда — ‹secret›») — пары нет (M1): литерал, значение не выходит. Контекст
/// сохранён — `exposed` пуст.
#[test]
fn restore_context_changed_exposed() {
    let p = pw(41);
    // 2: снят забор вокруг флага — строка флага та же
    let base = format!("# Стенд\n\n```bash\nmysql -p{p} db\n```\n");
    let view = mask_text(&base);
    let r = restore(&base, &view.replace("```bash\n", "").replace("```\n", ""));
    assert_eq!(r.exposed, vec![3], "{}", r.text);
    // забор сохранён, правка рядом — проходит
    let r = restore(&base, &view.replace("db\n", "db --verbose\n"));
    assert!(r.exposed.is_empty() && r.text.contains(&p), "{}", r.text);
    // 3: URL переписан прозой — слово перед меткой сменилось: литерал
    let base = format!("база: https://user:{p}@db.example.com/\n");
    let view = mask_text(&base);
    let r = restore(
        &base,
        &view
            .replace("https://user:", "логин user, пароль ")
            .replace("@db.example.com/", " (db.example.com)"),
    );
    assert!(
        !r.text.contains(&p) && r.exposed.is_empty() && r.kept == 1,
        "{}",
        r.text
    );
    // 3′: то же слово перед меткой, но не URL — значение вышло бы
    let r = restore(
        &base,
        &view
            .replace("https://user:", "user ")
            .replace("@db.example.com/", " db.example.com"),
    );
    assert_eq!(r.exposed, vec![1], "{}", r.text);
    // URL сохранён, правка вокруг — проходит
    let r = restore(&base, &view.replace("база:", "база prod:"));
    assert!(r.exposed.is_empty() && r.text.contains(&p), "{}", r.text);
    // 4: «password = ‹secret›» → «пароль от стенда — ‹secret›»: литерал
    let base = format!("password = {p}\n");
    let view = mask_text(&base);
    let r = restore(&base, &view.replace("password = ", "пароль от стенда — "));
    assert!(
        !r.text.contains(&p) && r.exposed.is_empty() && r.kept == 1,
        "{}",
        r.text
    );
    // 4′: разделитель снят — значение вышло бы
    let r = restore(&base, &view.replace("password = ", "password "));
    assert_eq!(r.exposed, vec![1], "{}", r.text);
    // разделитель сохранён — проходит
    let r = restore(&base, &view.replace("password = ", "password: "));
    assert!(r.exposed.is_empty() && r.text.contains(&p), "{}", r.text);
}

/// Пробы ревью 6–7 (M3): база с `\r\n`, правка с `\n` — значения на месте, окончания — как у
/// правки; многострочное значение — с окончаниями правки; правка с `\r\n` по базе `\n` — тоже.
#[test]
fn restore_crlf_normalized() {
    let k = key(51);
    let base = format!("# K\r\n\r\ntoken: {k}\r\nпрочее\r\n");
    let edited = mask_text(&base)
        .replace("\r\n", "\n")
        .replace("прочее", "прочее 2");
    let r = restore(&base, &edited);
    assert_eq!(r.text, format!("# K\n\ntoken: {k}\nпрочее 2\n"));
    assert_eq!(r.kept, 0);
    let mut base = String::new();
    for i in 0..5000 {
        base.push_str(&format!("token: {}\r\n", key(i)));
    }
    let mut edited = mask_text(&base).replace("\r\n", "\n");
    edited.push_str(&format!("новый {M}\n"));
    let r = restore(&base, &edited);
    assert_eq!(r.kept, 1);
    assert_eq!(
        (0..5000).filter(|&i| r.text.contains(&key(i))).count(),
        5000
    );
    assert!(!r.text.contains('\r'));
    // правка с `\r\n` по базе с `\n`
    let base = format!("token: {k}\nконец\n");
    let r = restore(&base, &format!("token: {M} (prod)\r\nконец\r\n"));
    assert_eq!(r.text, format!("token: {k} (prod)\r\nконец\r\n"));
    // многострочное значение (закрытый ключ) — окончания правки
    let pem = format!(
        "-----BEGIN {}PRIVATE KEY-----\r\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASC\r\n-----END {}PRIVATE KEY-----",
        "RSA ", "RSA "
    );
    let base = format!("ключ:\r\n{pem}\r\nконец\r\n");
    let view = mask_text(&base);
    assert!(view.contains(M), "{view}");
    let r = restore(
        &base,
        &view.replace("\r\n", "\n").replace("конец", "конец 2"),
    );
    assert_eq!(
        r.text,
        format!("ключ:\n{}\nконец 2\n", pem.replace("\r\n", "\n"))
    );
}

/// Проба ревью 7b: 5000 однообразных строк, каждая правлена (цитата), и одна новая метка —
/// подпись строк участка ставит их в пары по порядку.
#[test]
fn restore_quote_same_wording() {
    let mut base = String::new();
    for i in 0..5000 {
        base.push_str(&format!("token: {}\n", key(i)));
    }
    let mut edited: String = mask_text(&base)
        .split_inclusive('\n')
        .map(|l| format!("> {l}"))
        .collect();
    edited.push_str(&format!("новый {M}\n"));
    let r = restore(&base, &edited);
    assert_eq!(r.kept, 1);
    let restored = (0..5000)
        .filter(|&i| r.text.contains(&format!("> token: {}\n", key(i))))
        .count();
    assert_eq!(restored, 5000);
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
/// однообразная строка без номера (`token: <ключ>`, `kind` 5 — одна на секцию).
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

/// Правка строки: суффикс до перевода строки или префикс.
fn edit_line(s: &mut String, e: usize) {
    let end = s.trim_end_matches(['\r', '\n']).len();
    if e.is_multiple_of(2) {
        s.insert_str(end, &format!(" (правка {e})"));
    } else {
        s.insert_str(0, "- ");
    }
}

/// Случайные правки агента по виду: вставка, удаление, правка до и после меток, перенос строки
/// (и перенос правленой), однообразные строки в разных секциях, база с `\r\n` и правка с `\n`,
/// смена окружения метки (`password = ‹secret›` → `password ‹secret›`). Итог — ровно те же правки
/// исходника (окончания правки), меток без пары нет; строки со сменой окружения — ровно в
/// `exposed`. Зерно и число случаев — `T6_SEED`, `T6_CASES`.
#[test]
fn restore_random_line_edits() {
    let env = |k: &str| std::env::var(k).ok().and_then(|s| s.parse::<u64>().ok());
    let mut rng = Rng(env("T6_SEED").unwrap_or(7));
    for case in 0..env("T6_CASES").unwrap_or(1000) as u32 {
        let crlf = rng.next(4) == 0;
        // секции с заголовком; однообразная строка — не больше одной на секцию
        let mut raw: Vec<String> = Vec::new();
        for s in 0..1 + rng.next(6) {
            raw.push(format!("## Секция {case}-{s}\n"));
            let mut same = false;
            for _ in 0..1 + rng.next(8) {
                let i = case * 1000 + raw.len() as u32;
                let mut kind = rng.next(6);
                if kind == 5 && same {
                    kind = 0;
                }
                same |= kind == 5;
                raw.push(line(i, kind));
            }
        }
        if crlf {
            for l in &mut raw {
                *l = l.replace('\n', "\r\n");
            }
        }
        let base: String = raw.concat();
        let masked = mask_text(&base);
        let mut seen: Vec<String> = masked.split_inclusive('\n').map(str::to_owned).collect();
        assert_eq!(raw.len(), seen.len(), "строки вида — как строки исходника");
        if crlf {
            // клиент агента привёл переводы строк к `\n`; ожидаемый итог — тоже
            for l in raw.iter_mut().chain(seen.iter_mut()) {
                *l = l.replace("\r\n", "\n");
            }
        }
        // (правлена, перенесена, окружение снято); однообразные строки не переносятся
        let mut done: Vec<(bool, bool, bool)> = raw
            .iter()
            .map(|l| {
                (
                    l.starts_with("## "),
                    l.starts_with("## ") || l.starts_with("token: "),
                    false,
                )
            })
            .collect();
        for e in 0..1 + rng.next(7) {
            let len = raw.len();
            match rng.next(6) {
                0 => {
                    let at = rng.next(len + 1);
                    let t = format!("вставка {case}-{e}\n");
                    raw.insert(at, t.clone());
                    seen.insert(at, t);
                    done.insert(at, (true, true, false));
                }
                1 if len > 1 => {
                    let at = rng.next(len);
                    if raw[at].starts_with("## ") {
                        continue;
                    }
                    raw.remove(at);
                    seen.remove(at);
                    done.remove(at);
                }
                2 | 3 => {
                    let at = rng.next(len);
                    if done[at].0 || done[at].2 {
                        continue;
                    }
                    edit_line(&mut raw[at], e);
                    edit_line(&mut seen[at], e);
                    done[at].0 = true;
                }
                4 => {
                    // смена окружения: ключ-значение без разделителя (слово перед меткой то же)
                    let at = rng.next(len);
                    if done[at].0 || !seen[at].ends_with(&format!("password = {M}\n")) {
                        continue;
                    }
                    raw[at] = raw[at].replace("password = ", "password ");
                    seen[at] = seen[at].replace("password = ", "password ");
                    done[at] = (true, true, true);
                }
                _ => {
                    let from = rng.next(len);
                    if done[from].1 {
                        continue;
                    }
                    let (r, s, d) = (raw.remove(from), seen.remove(from), done.remove(from));
                    let to = rng.next(len);
                    raw.insert(to, r);
                    seen.insert(to, s);
                    done.insert(to, (d.0, true, d.2));
                }
            }
        }
        let (want, edited) = (raw.concat(), seen.concat());
        let r = restore(&base, &edited);
        assert_eq!(r.text, want, "случай {case}:\n{edited}");
        assert_eq!(r.kept, 0, "случай {case}");
        let exposed: Vec<usize> = done
            .iter()
            .enumerate()
            .filter(|(_, d)| d.2)
            .map(|(i, _)| i + 1)
            .collect();
        assert_eq!(r.exposed, exposed, "случай {case}:\n{edited}");
    }
}

/// Каждая строка большой заметки правлена (цитата `> `) — все значения на месте.
#[test]
fn restore_every_line_rewritten() {
    let lines: Vec<String> = (0..6000).map(|i| line(i, (i % 5) as usize)).collect();
    let base = lines.concat();
    let edited: String = mask_text(&base)
        .split_inclusive('\n')
        .map(|l| format!("> {l}"))
        .collect();
    let want: String = lines.iter().map(|l| format!("> {l}")).collect();
    let r = restore(&base, &edited);
    assert_eq!(r.kept, 0);
    assert!(r.text == want, "значения не на месте");
}

/// Большой изменённый участок с другим числом строк (строки склеены попарно) и тем же числом
/// меток — значения на месте.
#[test]
fn restore_big_block_ordered() {
    let lines: Vec<String> = (0..3000).map(|i| line(i, 1 + (i % 4) as usize)).collect();
    let base = lines.concat();
    let seen: Vec<String> = mask_text(&base)
        .split_inclusive('\n')
        .map(str::to_owned)
        .collect();
    let join = |v: &[String]| -> String {
        v.chunks(2)
            .map(|c| {
                c.iter()
                    .map(|l| l.trim_end())
                    .collect::<Vec<_>>()
                    .join(" | ")
                    + "\n"
            })
            .collect()
    };
    let r = restore(&base, &join(&seen));
    assert_eq!(r.kept, 0);
    assert!(r.text == join(&lines), "значения не по порядку");
}

/// Цена восстановления (журнал этапа, задача 6 M7.1): заметка 1 МБ и 5 МиБ — разделы с прозой,
/// кодом и ключом на ~157 байт; правка агента — дописка к строкам с метками в десяти местах,
/// вставка и удаление строк; худший случай — правлена каждая строка. Медиана 5 прогонов, только
/// release: `cargo test --release -p zam-secrets --lib -- --ignored bench_restore --nocapture`.
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
    for size in [1024 * 1024, 5 * 1024 * 1024] {
        let base = doc(size);
        let masked = mask_text(&base);
        let mut lines: Vec<String> = masked.split_inclusive('\n').map(str::to_owned).collect();
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
        let all: String = masked
            .split_inclusive('\n')
            .map(|l| format!("> {l}"))
            .collect();
        for (what, edited) in [("правка", &small), ("каждая строка", &all)] {
            let mut times = Vec::new();
            let mut kept = 0;
            for _ in 0..5 {
                let t0 = std::time::Instant::now();
                kept = restore(&base, edited).kept;
                times.push(t0.elapsed());
            }
            times.sort();
            println!(
                "восстановление {} байт ({what}, {} меток, без пары {kept}): медиана {:?}, \
                 мин {:?}, макс {:?}",
                base.len(),
                edited.matches(M).count(),
                times[2],
                times[0],
                times[4]
            );
        }
    }
}
