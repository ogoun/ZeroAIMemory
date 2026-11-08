//! Вес фрагментов вложений `search.w_attach` (M6 §11, задача 11): при 1,0 — выдача побайтово как
//! в v0.0.9 (эталон снят кодом до задачи 11), иначе — множитель балла фрагментов вложений.
//!
//! Отдельный файл от `attachments_search`: тесты с моделью ставят счётчик токенов чанкера на
//! весь процесс (`zam_store::set_token_counter`), и мир без модели в том же процессе резал бы
//! DOCX её токенизатором (1 фрагмент → 3) — выдача зависела бы от порядка тестов. Здесь модели
//! нет: чанкер всегда режет `ApproxCounter`, как при снятии эталона.

mod common;

use axum::http::{Method, StatusCode};
use common::api::Api;
use common::attach::{chunk_locators, docx, items, pptx, search, upload, wait_done, xlsx};
use serde_json::{Value, json};

/// Заметка токеном `token`; `area` — заголовок `X-Zam-Area`.
async fn note(
    a: &Api,
    token: &str,
    space: &str,
    path: &str,
    content: &str,
    area: Option<&str>,
) -> String {
    let extra: Vec<(&str, &str)> = area.map(|v| ("x-zam-area", v)).into_iter().collect();
    let r = a
        .tok_h(
            token,
            Method::POST,
            "/api/v1/nodes",
            Some(json!({"space": space, "path": path, "kind": "note", "content": content})),
            &extra,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    r.json()["id"].as_str().unwrap().to_string()
}

// ------------------------------------------------------------- M6 §11: вес вложений w_attach

/// Эталон выдачи до задачи 11 (M6 §11, `search.w_attach`): мир FTS (без модели). Гибридный мир
/// теста невоспроизводим между двумя построениями даже на прежнем коде: ничьи косинусов (сводка
/// и лист с одним текстом) упорядочиваются по случайным `chunk_id` загрузки (методика плана M6,
/// правило 3; проверено: два снятия кода v0.0.9 — 218 строк эталона разные). Побайтовую сверку
/// гибрида делает eval документов на одном индексе (`--reuse`, журнал задачи 11).
const W_ATTACH_FIXTURE_FTS: &str = "tests/fixtures/w_attach_m57_fts.json";

/// Заметки мира вложений: темы эксплуатации, частью те же, что у документов (вложения и заметки
/// соревнуются в выдаче).
const W_NOTES: &[(&str, &str)] = &[
    (
        "ops/pump-bearings",
        "# Подшипники насоса\n\nЗамена подшипников главного насоса — по вибрации и наработке в моточасах; журнал вибрации ведёт смена.\n",
    ),
    (
        "ops/filter-backwash",
        "# Промывка фильтра\n\nФильтр грубой очистки промывают обратным током раз в смену; перепад давления — признак засора.\n",
    ),
    (
        "ops/generator-test",
        "# Дизель-генератор\n\nРезервный генератор проверяют под нагрузкой ежемесячно, протокол — в папке энергетика.\n",
    ),
    (
        "ops/valve-lockout",
        "# Блокировка задвижек\n\nПеред ремонтом задвижку закрывают, вешают замок и бирку; ключ — у ответственного.\n",
    ),
    (
        "ops/compressor-oil",
        "# Масло компрессора\n\nУровень масла компрессора смотрят на остановленной машине; доливают маслом той же марки.\n",
    ),
    (
        "ops/boiler-water",
        "# Вода котла\n\nЖёсткость питательной воды котла меряют ежедневно; превышение — продувка и проверка умягчителя.\n",
    ),
    (
        "ops/conveyor-belt",
        "# Лента конвейера\n\nСход ленты конвейера правят натяжной станцией; износ кромки — повод заказать ленту.\n",
    ),
    (
        "ops/forklift-charge",
        "# Зарядка погрузчиков\n\nТяговые батареи погрузчиков заряжают в отдельном помещении с вытяжкой, полный цикл — восемь часов.\n",
    ),
    (
        "it/backup-rotation",
        "# Ротация резервных копий\n\nКопии сервера учёта хранят по схеме дед-отец-сын: дневные неделю, недельные месяц, месячные год.\n",
    ),
    (
        "it/printer-queue",
        "# Очередь печати\n\nЗависшую очередь печати сбрасывают перезапуском службы диспетчера печати.\n",
    ),
    (
        "it/vpn-access",
        "# Удалённый доступ\n\nДоступ к VPN выдаёт отдел ИТ по заявке руководителя; ключи действуют год.\n",
    ),
    (
        "hr/shift-handover",
        "# Передача смены\n\nПри передаче смены старший расписывается в журнале и называет незакрытые наряды.\n",
    ),
    (
        "hr/ppe-issue",
        "# Средства защиты\n\nКаски, очки и перчатки выдают по ведомости; срок носки перчаток — месяц.\n",
    ),
    (
        "safety/fire-drill",
        "# Учебная тревога\n\nУчебную пожарную тревогу проводят раз в полгода, сбор — у главной проходной.\n",
    ),
    (
        "safety/hot-work",
        "# Огневые работы\n\nНа огневые работы оформляют наряд-допуск, место очищают от горючего, рядом — огнетушитель.\n",
    ),
    (
        "quality/sample-retention",
        "# Хранение проб\n\nКонтрольные пробы партии хранят три месяца с этикеткой даты и номера партии.\n",
    ),
];

/// Запросы мира вложений: слова документов, слова заметок, общие темы и пересказ.
const W_QUERIES: &[&str] = &[
    "замена подшипников главного насоса",
    "промывка фильтра обратным током",
    "проверка генератора под нагрузкой",
    "остаток подшипников на складе",
    "график внедрения системы учёта",
    "сколько хранить контрольные пробы",
    "как часто меняют подшипники",
    "перепад давления на фильтре",
    "ротация резервных копий",
    "срок носки перчаток",
];

/// Мир M6 §11: 16 заметок и три документа (DOCX — регламент, XLSX — склад, PPTX — план) одной
/// заметки-владельца, только FTS (без модели); без затухания по свежести (баллы не зависят от
/// времени между снятиями).
async fn w_attach_world(
    name: &str,
    tweak: impl FnOnce(&mut zam_server::config::ServerConfig),
) -> common::attach::World {
    let w = common::attach::world_with(name, false, |c| {
        c.search.recency_half_life_days = 0.0;
        tweak(c);
    })
    .await;
    let a = &w.a;
    let user = a.st.admin_id;
    for (path, body) in W_NOTES {
        note(a, &a.token, "knowledge", path, body, None).await;
    }
    let owner = note(
        a,
        &a.token,
        "knowledge",
        "docs/регламенты",
        "# Регламенты\n\nДокументы службы эксплуатации.\n",
        None,
    )
    .await;
    let files: Vec<(&str, Vec<u8>)> = vec![
        (
            "регламент.docx",
            docx(&[
                "Замена подшипников главного насоса выполняется после 8000 моточасов или при росте вибрации выше 7 мм/с.",
                "Фильтр грубой очистки промывается обратным током при перепаде давления 0,5 бар.",
                "Резервный дизель-генератор проверяется под нагрузкой не реже одного раза в месяц.",
                "Методику измерения вибраций согласовали с главным механиком и службой надёжности.",
                "Контрольные пробы каждой партии хранятся девяносто дней в лаборатории.",
                "Огневые работы на складе масел запрещены без письменного наряда-допуска.",
            ]),
        ),
        (
            "склад.xlsx",
            xlsx(&[(
                "Остатки",
                &[
                    &["Позиция", "Количество", "Место"],
                    &["Подшипник 6312", "14", "стеллаж А3"],
                    &["Сальник насоса", "40", "стеллаж А4"],
                    &["Фильтроэлемент ФГО", "6", "стеллаж Б1"],
                    &["Перчатки рабочие", "300", "кладовая"],
                ],
            )]),
        ),
        (
            "план.pptx",
            pptx(&[
                (
                    "План внедрения",
                    "Система учёта ремонтов: пилот в цехе насосов",
                ),
                (
                    "Сроки",
                    "График внедрения: пилот в марте, весь завод к сентябрю",
                ),
                ("Риски", "Нет резервных копий базы учёта до перехода"),
            ]),
        ),
    ];
    for (file, data) in &files {
        upload(a, &a.token, &owner, file, data).await;
    }
    for (file, _) in &files {
        let d = wait_done(a, user, &owner, file).await;
        assert!(!chunk_locators(a, user, d.id).await.is_empty(), "{file}");
    }
    w
}

/// Выдача запросов мира: по запросу — вид, путь, файл, `locator`, каналы и балл каждой карточки
/// (смешанная выдача и одни вложения).
async fn w_attach_answers(a: &Api) -> Value {
    let card = |i: &Value| {
        json!({
            "kind": i["kind"], "path": i["path"], "file": i["file"], "locator": i["locator"],
            "sources": i["sources"], "score": i["score"],
        })
    };
    let mut out = Vec::new();
    for q in W_QUERIES {
        let body = json!({"query": q, "top_k": 10, "budget_tokens": 100000});
        let mixed = search(a, &a.token, body.clone()).await;
        let mut only = body;
        only["filters"] = json!({"kinds": ["attachment"]});
        let only = search(a, &a.token, only).await;
        out.push(json!({
            "query": q,
            "mixed": items(&mixed).iter().map(card).collect::<Vec<_>>(),
            "attach": items(&only).iter().map(card).collect::<Vec<_>>(),
        }));
    }
    json!(out)
}

fn w_fixture(rel: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// Снимает эталон [`W_ATTACH_FIXTURE_FTS`] (код до задачи 11).

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "снимает эталон; запускать только на коде до правки M6 §11 (w_attach)"]
async fn snapshot_w_attach_fixture() {
    let w = w_attach_world("watt-snap-fts", |_| {}).await;
    let v = json!({"platform": std::env::consts::OS, "answers": w_attach_answers(&w.a).await});
    std::fs::write(
        w_fixture(W_ATTACH_FIXTURE_FTS),
        serde_json::to_string_pretty(&v).unwrap() + "\n",
    )
    .unwrap();
    w.a.st.shutdown().await;
}

fn w_load(rel: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(w_fixture(rel)).unwrap()).unwrap()
}

/// `search.w_attach = 1,0` (умолчание) — выдача побайтово как эталон до задачи 11: вид, путь,
/// файл, `locator`, каналы и балл каждой карточки десяти запросов (смешанная выдача и одни
/// вложения) в мире из 17 заметок и трёх документов (DOCX, XLSX, PPTX), FTS (гибрид — см.
/// [`W_ATTACH_FIXTURE_FTS`]).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn w_attach_1_byte_equal_m57() {
    let want = w_load(W_ATTACH_FIXTURE_FTS);
    let w = w_attach_world("watt-eq-fts", |c| c.search.w_attach = 1.0).await;
    let got = w_attach_answers(&w.a).await;
    // мир не пустой: вложения есть и в смешанной выдаче, и в выдаче одних вложений
    let n_att: usize = got
        .as_array()
        .unwrap()
        .iter()
        .map(|q| {
            q["mixed"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|c| c["kind"] == "attachment")
                .count()
        })
        .sum();
    assert!(n_att >= 5, "{got}");
    if got != want["answers"] {
        // выдача — рядом с эталоном для сравнения (каталог тестов проекта)
        let d = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-tmp");
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("w_attach_m57_fts.got.json");
        std::fs::write(
            &p,
            serde_json::to_string_pretty(&json!({"answers": got})).unwrap(),
        )
        .unwrap();
        panic!("выдача FTS отличается от эталона: {}", p.display());
    }
    w.a.st.shutdown().await;
}

/// `w_attach` — множитель балла фрагментов вложений: при 0,5 у одних вложений тот же порядок и
/// баллы вдвое ниже эталона (балл ответа — 4 знака), карточки заметок смешанной выдачи — с прежними баллами; при 0
/// все вложения смешанной выдачи — после всех заметок.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn w_attach_scales_attachment_scores() {
    let want = w_load(W_ATTACH_FIXTURE_FTS);
    let w = w_attach_world("watt-half", |c| c.search.w_attach = 0.5).await;
    let got = w_attach_answers(&w.a).await;
    let key = |c: &Value| {
        (
            c["kind"].clone(),
            c["path"].clone(),
            c["file"].clone(),
            c["locator"].clone(),
        )
    };
    let mut seen_attach = 0;
    for (g, e) in got
        .as_array()
        .unwrap()
        .iter()
        .zip(want["answers"].as_array().unwrap())
    {
        let (ga, ea) = (
            g["attach"].as_array().unwrap(),
            e["attach"].as_array().unwrap(),
        );
        assert_eq!(
            ga.iter().map(key).collect::<Vec<_>>(),
            ea.iter().map(key).collect::<Vec<_>>(),
            "{}",
            g["query"]
        );
        for (x, y) in ga.iter().zip(ea) {
            let (x, y) = (x["score"].as_f64().unwrap(), y["score"].as_f64().unwrap());
            assert!((x - y * 0.5).abs() <= 1e-4, "{}: {x} vs {y}", g["query"]);
            seen_attach += 1;
        }
        let notes_of = |v: &Value| -> Vec<(Value, Value)> {
            v["mixed"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|c| c["kind"] == "note")
                .map(|c| (c["path"].clone(), c["score"].clone()))
                .collect()
        };
        // заметки, бывшие в эталоне, — с тем же баллом (вложения ниже не вытесняют их вверх)
        let gn = notes_of(g);
        for n in notes_of(e) {
            if let Some(x) = gn.iter().find(|x| x.0 == n.0) {
                assert_eq!(x.1, n.1, "{}: {:?}", g["query"], n.0);
            }
        }
    }
    assert!(seen_attach >= 5, "мир без вложений в выдаче: {got}");
    w.a.st.shutdown().await;

    let w = w_attach_world("watt-zero", |c| c.search.w_attach = 0.0).await;
    for g in w_attach_answers(&w.a).await.as_array().unwrap() {
        let kinds: Vec<&str> = g["mixed"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["kind"].as_str().unwrap())
            .collect();
        if let Some(first_att) = kinds.iter().position(|k| *k == "attachment") {
            assert!(
                kinds[first_att..].iter().all(|k| *k == "attachment"),
                "{}: {kinds:?}",
                g["query"]
            );
        }
    }
    w.a.st.shutdown().await;
}
