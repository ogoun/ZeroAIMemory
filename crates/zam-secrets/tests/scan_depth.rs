//! Счётчик повторов скана и упор в `FIXPOINT_MAX` (задача 15b M6, долг M5.6): упор больше не
//! молчит — [`scan_secrets_ext`] возвращает `passes` и `depth_capped`.
//!
//! Вход, на котором скан упирается в предел, подобран генератором [`generate_depth_capped`]
//! (эволюционный перебор склеек атомов `common::atoms` и минимизация) и закреплён фикстурой
//! `fixtures/scan_depth_capped.txt` — шаблоном `common::expand` (секреты в файле не хранятся:
//! GitHub push protection). Цепочка находок на нём (``x://pwd=:`/<40>@pwd=://<20>"pwd=:`@``):
//! 0. ключ-значение — значение второго `pwd=` (`://<20>`, до `"`);
//! 1. энтропия `/<40>` — в URL-токене больше нет `://`;
//! 2. пароль в URL `x://pwd=:` — пароль `` `‹secret›`` (в тексте мешал `/`);
//! 3. ключ-значение первого `pwd=` — значение `:‹secret›@…` (в тексте `:` до `` ` `` — короче 6);
//! 4. пароль в URL — `` ` `` последнего ``pwd=:`@`` (имя пользователя теперь без `:`);
//! 5. ключ-значение последнего `pwd=` — снова; звено ``"pwd=:`@`` удлиняет цепочку на два
//!    повтора.

mod common;

use common::{atoms, corpus, expand};
use zam_secrets::{FIXPOINT_MAX, mask_text, scan_secrets, scan_secrets_ext};

/// Закреплённый вход упора (шаблон фикстуры, раскрытый).
fn depth_capped_input() -> String {
    let t = include_str!("fixtures/scan_depth_capped.txt");
    expand(t.trim_end_matches(['\r', '\n']))
}

/// Вход генератора упирается в предел: все [`FIXPOINT_MAX`] повторов дали находки, и
/// проверочный повтор тоже; находки — те же, что у `scan_secrets`.
#[test]
fn scan_depth_capped() {
    let t = depth_capped_input();
    let o = scan_secrets_ext(&t);
    assert!(o.depth_capped, "упор не обнаружен: passes = {}", o.passes);
    assert_eq!(o.passes, FIXPOINT_MAX);
    assert_eq!(
        o.hits,
        scan_secrets(&t),
        "проверочный повтор не меняет находок"
    );
    // признак честный: маскировка на таком входе не идемпотентна
    let once = mask_text(&t);
    assert_ne!(mask_text(&once), once, "упор без потери идемпотентности");
    // на звено короче — ровно предел, без упора: проверочный повтор ничего не нашёл
    let short = t
        .strip_suffix("\"pwd=:`@")
        .expect("звено цепочки в конце фикстуры");
    let o = scan_secrets_ext(short);
    assert_eq!((o.passes, o.depth_capped), (FIXPOINT_MAX, false), "{short}");
}

/// Обычные входы (корпус тестов сканера) укладываются в 3 повтора и не упираются в предел.
#[test]
fn ordinary_inputs_le_3_passes() {
    let mut max = 0;
    for t in corpus() {
        let o = scan_secrets_ext(&t);
        assert!(o.passes <= 3, "{} повторов: {t:?}", o.passes);
        assert!(!o.depth_capped, "{t:?}");
        assert_eq!(o.hits, scan_secrets(&t));
        max = max.max(o.passes);
    }
    // корпус задевает повторы: секреты есть, повтор по виду выполняется
    assert!(max >= 1, "корпус без повторов");
}

/// Без находок повторов нет; вид из одних масок второй раз не сканируется; текст с секретом
/// без каскада — один повтор (он ничего не нашёл).
#[test]
fn passes_counted() {
    assert_eq!(scan_secrets_ext("обычный текст").passes, 0);
    let key = common::keys()[2].clone();
    let o = scan_secrets_ext(&key);
    assert_eq!((o.passes, o.depth_capped, o.hits.len()), (0, false, 1));
    let o = scan_secrets_ext("x password=abcdef123 y");
    assert_eq!((o.passes, o.depth_capped, o.hits.len()), (1, false, 1));
}

/// Генератор входа упора: эволюционный перебор склеек атомов (вставка, удаление, замена,
/// перестановка, повтор куска, скрещивание; приспособленность — число повторов, упор — цель) и
/// минимизация найденного удалением токенов (delta debugging). Печатает шаблон для фикстуры.
/// Случайный перебор без отбора до упора не доходит: на 4,6 млн склеек до 24 атомов — 12 входов
/// с тремя повторами и ни одного с четырьмя. `cargo test -p zam-secrets --release --test
/// scan_depth -- --ignored generate_depth_capped --nocapture`.
#[test]
#[ignore]
fn generate_depth_capped() {
    let atoms = atoms();
    let na = atoms.len();
    let tpl = |v: &[usize]| v.iter().map(|&i| atoms[i].as_str()).collect::<String>();
    let score = |t: &str| {
        let o = scan_secrets_ext(&expand(t));
        i64::from(o.passes) * 10 + if o.depth_capped { 100 } else { 0 }
    };
    // перезапуски с новым зерном: отбор иногда застревает на трёх повторах
    let mut found = None;
    for restart in 0..64u64 {
        if found.is_some() {
            break;
        }
        let mut x: u64 = 0x9E37_79B9 ^ restart;
        let mut r = move |n: usize| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x % n as u64) as usize
        };
        let mut pop: Vec<(i64, Vec<usize>)> = (0..200)
            .map(|_| {
                let v: Vec<usize> = (0..6 + r(12)).map(|_| r(na)).collect();
                (score(&tpl(&v)), v)
            })
            .collect();
        for _ in 0..300_000 {
            let mut v = pop[r(pop.len())].1.clone();
            match r(6) {
                0 => {
                    let p = r(v.len() + 1);
                    v.insert(p, r(na));
                }
                1 if v.len() > 2 => {
                    let p = r(v.len());
                    v.remove(p);
                }
                2 => {
                    let p = r(v.len());
                    v[p] = r(na);
                }
                3 => {
                    let b = pop[r(pop.len())].1.clone();
                    let cut = r(b.len());
                    let p = r(v.len() + 1);
                    v.splice(p..p, b[cut..].iter().copied());
                }
                4 => {
                    let (p, q) = (r(v.len()), r(v.len()));
                    v.swap(p, q);
                }
                _ => {
                    let p = r(v.len());
                    let l = 1 + r(3.min(v.len() - p));
                    let seg = v[p..p + l].to_vec();
                    v.splice(p..p, seg);
                }
            }
            if v.len() > 40 {
                continue;
            }
            let f = score(&tpl(&v)) - v.len() as i64 / 8;
            let (i, j) = (r(pop.len()), r(pop.len()));
            let w = if pop[i].0 < pop[j].0 { i } else { j };
            if f >= pop[w].0 {
                pop[w] = (f, v.clone());
            }
            if f >= 100 {
                found = Some(tpl(&v));
                break;
            }
        }
    }
    let found = found.expect("упор не найден");
    // минимизация: токены — шаблоны `{…}` и отдельные символы
    let mut toks: Vec<String> = Vec::new();
    let mut rest = found.as_str();
    while let Some(c) = rest.chars().next() {
        let n = if c == '{' {
            rest.find('}').unwrap() + 1
        } else {
            c.len_utf8()
        };
        toks.push(rest[..n].to_string());
        rest = &rest[n..];
    }
    let capped = |t: &[String]| scan_secrets_ext(&expand(&t.concat())).depth_capped;
    let mut chunk = toks.len() / 2;
    while chunk >= 1 {
        let mut i = 0;
        while i < toks.len() {
            let mut c = toks.clone();
            c.drain(i..(i + chunk).min(toks.len()));
            if capped(&c) {
                toks = c;
            } else {
                i += chunk;
            }
        }
        chunk /= 2;
    }
    println!(
        "найдено: {found}\nминимум ({} токенов): {}",
        toks.len(),
        toks.concat()
    );
}
