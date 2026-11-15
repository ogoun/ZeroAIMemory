//! Сканер секретов §7.6: правила, отсутствие ложных срабатываний на идентификаторах,
//! маскирование только значения, property-тест целостности текста.
//!
//! Тестовые «секреты» собираются из частей во время выполнения: в исходниках нет строк,
//! похожих на настоящие ключи (сканеры утечек репозитория).

mod common;

use common::rnd;
use proptest::prelude::*;
use zam_secrets::{SECRET_MARK, SecretKind, mask_secrets, mask_text, scan_secrets};

fn kinds(text: &str) -> Vec<SecretKind> {
    scan_secrets(text).into_iter().map(|h| h.kind).collect()
}

fn one(text: &str) -> SecretKind {
    let k = kinds(text);
    assert_eq!(k.len(), 1, "ожидался один хит в {text:?}: {k:?}");
    k[0]
}

#[test]
fn detects_each_rule() {
    let up = |n, s| {
        rnd(n, s)
            .to_ascii_uppercase()
            .replace(|c: char| !c.is_ascii_alphanumeric(), "A")
    };
    // AWS
    let aws = format!("{}{}", "AK", "IA") + &up(16, 1);
    assert!(matches!(
        one(&format!("key {aws} end")),
        SecretKind::KnownPrefix("aws")
    ));
    let aws2 = format!("{}{}", "AS", "IA") + &up(16, 2);
    assert!(matches!(one(&aws2), SecretKind::KnownPrefix("aws")));
    // GitHub
    for p in ["ghp", "gho", "ghu", "ghs", "ghr"] {
        let t = format!("{p}_{}", rnd(36, 3));
        assert!(
            matches!(one(&format!("x {t}")), SecretKind::KnownPrefix("github")),
            "{p}"
        );
    }
    let pat = format!("github{}pat_{}", "_", rnd(70, 4));
    assert!(matches!(one(&pat), SecretKind::KnownPrefix("github")));
    // GitLab, Slack, sk-, Google
    assert!(matches!(
        one(&format!("gl{}-{}", "pat", rnd(24, 5))),
        SecretKind::KnownPrefix("gitlab")
    ));
    assert!(matches!(
        one(&format!("xo{}-{}", "xb", "1234567890-abcdefghij")),
        SecretKind::KnownPrefix("slack")
    ));
    assert!(matches!(
        one(&format!("s{}-{}", "k", rnd(30, 6))),
        SecretKind::KnownPrefix("api")
    ));
    assert!(matches!(
        one(&format!("s{}-ant-{}", "k", rnd(30, 7))),
        SecretKind::KnownPrefix("api")
    ));
    assert!(matches!(
        one(&format!("AI{}{}", "za", rnd(35, 8))),
        SecretKind::KnownPrefix("google")
    ));
    // Hugging Face, npm, SendGrid (ревью задачи 5 M5.6, M8)
    assert!(matches!(
        one(&format!("token {}{}", "hf_", rnd(34, 30))),
        SecretKind::KnownPrefix("huggingface")
    ));
    assert!(matches!(
        one(&format!("authToken {}{}", "npm_", rnd(36, 31))),
        SecretKind::KnownPrefix("npm")
    ));
    assert!(matches!(
        one(&format!("key {}.{}.{}", "SG", rnd(22, 32), rnd(43, 33))),
        SecretKind::KnownPrefix("sendgrid")
    ));
    // токен ZeroAIMemory (формат zam_core::token)
    let zam = format!("za{}_{}", "m", rnd(40, 9));
    assert!(matches!(
        one(&format!("Authorization: Bearer {zam}")),
        SecretKind::KnownPrefix("zeroaimemory")
    ));
    // JWT
    let jwt = format!("eyJ{}.{}.{}", rnd(20, 10), rnd(30, 11), rnd(20, 12));
    assert_eq!(one(&format!("Bearer {jwt}")), SecretKind::Jwt);
    // закрытый ключ
    let pk = format!(
        "-----BEGIN {} PRIVATE KEY-----\n{}\n-----END {} PRIVATE KEY-----",
        "RSA",
        rnd(64, 13),
        "RSA"
    );
    assert_eq!(one(&pk), SecretKind::PrivateKey);
    // ключ-значение
    assert_eq!(one("password = hunter2hunter2"), SecretKind::KeyValue);
    assert_eq!(one("api_key: abcdef123456"), SecretKind::KeyValue);
    assert_eq!(
        one(r#"{"client_secret": "s3cr3t-value"}"#),
        SecretKind::KeyValue
    );
    // пароль в URL
    assert_eq!(
        one("postgres://admin:s3cr3tpw@db.example.org:5432/app"),
        SecretKind::UrlPassword
    );
    // высокая энтропия
    let he = rnd(40, 14);
    assert!(
        he.bytes().any(|b| b.is_ascii_lowercase()) && he.bytes().any(|b| b.is_ascii_uppercase())
    );
    assert_eq!(one(&format!("value {he} here")), SecretKind::HighEntropy);
}

#[test]
fn ignores_identifiers() {
    let sha1 = "9fceb02d0ae598e95dc970b74767f19372d61af8";
    let sha256 = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    let uuid = "550e8400-e29b-41d4-a716-446655440000";
    let uuid_up = "550E8400-E29B-41D4-A716-446655440000";
    let id = zam_core::Id::new().to_string();
    let url = "https://example.com/search/results/page?q=rust+regex&utm_source=newsletter&ref=abcdefghijklmnopqrstuvwxyz0123456789ABCD";
    let samples = [
        format!("commit {sha1} fixes"),
        format!("sha256:{sha256}"),
        format!("sha256 {}", sha256.to_ascii_uppercase()),
        format!("id {uuid} / {uuid_up}"),
        format!("node {id} и ещё {id}"),
        format!("ссылка {url}"),
        format!("log: 2026-01-01T10:00:00Z hash={sha1} blake3={sha256}"),
        format!("integrity sha512-{}", rnd(86, 20)),
        "password=<your-password>".to_string(),
        "token: ${TOKEN}".to_string(),
        "secret = {{ vault.secret }}".to_string(),
        "api_key: ********".to_string(),
        "password: xxxxxxxx".to_string(),
        "password: changeme".to_string(),
        "token: Example".to_string(),
        "password: $DB_PASSWORD".to_string(),
        "src/components/UserProfileCard/Details2/ProfileCardDetailsView.tsx".to_string(),
        "ВерблюжийРегистрДлинныйИдентификаторБезЦифр".to_string(),
        // имена с префиксами новых правил — не секреты
        "fn hf_model_loader_from_pretrained_v2() npm_package_version SG.Config.load".to_string(),
        "pub fn handle_incoming_websocket_message_v2(&self) -> useQueryClientInvalidationOnMount2x"
            .to_string(),
        "обычный текст про токены и пароли: без значений".to_string(),
    ];
    for s in &samples {
        assert!(
            scan_secrets(s).is_empty(),
            "ложное срабатывание: {s:?} -> {:?}",
            scan_secrets(s)
        );
        assert_eq!(&mask_text(s), s);
    }
    // base32-id: все 26 символов Crockford, много раз подряд
    for _ in 0..200 {
        let id = zam_core::Id::new().to_string();
        assert!(scan_secrets(&id).is_empty(), "{id}");
    }
}

#[test]
fn masks_only_value_in_key_value() {
    let t = "config:\n  password = hunter2hunter2 # prod\nnext line";
    let hits = scan_secrets(t);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].line, 2);
    assert_eq!(&t[hits[0].range.clone()], "hunter2hunter2");
    let m = mask_secrets(t, &hits);
    assert_eq!(
        m,
        format!("config:\n  password = {SECRET_MARK} # prod\nnext line")
    );
    // значение в кавычках: кавычки остаются
    assert_eq!(
        mask_text(r#"db_password: "p@ssw0rd-123""#),
        format!(r#"db_password: "{SECRET_MARK}""#)
    );
    // пароль в URL — только пароль
    assert_eq!(
        mask_text("redis://user:topsecret99@host:6379/0"),
        format!("redis://user:{SECRET_MARK}@host:6379/0")
    );
}

#[test]
fn url_token_boundary_in_one_line_json() {
    // URL рядом не должен «закрывать» энтропийное правило для соседнего ключа
    let k = rnd(40, 60);
    let t = format!(r#"{{"url":"https://a.example/c","k":"{k}"}}"#);
    let hits = scan_secrets(&t);
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(&t[hits[0].range.clone()], k);
    // а кандидат внутри самого URL по-прежнему не маскируется
    let url = format!("(https://a.example/{}/x)", rnd(40, 61));
    assert!(scan_secrets(&url).is_empty());
}

#[test]
fn minor_rules() {
    // пользователь в URL может быть пустым
    assert_eq!(
        mask_text("redis://:pw123456@host:6379"),
        format!("redis://:{SECRET_MARK}@host:6379")
    );
    // значение в кавычках — до закрывающей кавычки
    assert_eq!(
        mask_text(r#"password: "correct horse battery" ok"#),
        format!(r#"password: "{SECRET_MARK}" ok"#)
    );
    // расширенный словарь ключей
    for k in ["secret_key", "SECRET_KEY", "signing_key", "private-key"] {
        assert_eq!(
            one(&format!("{k} = abcdef123456")),
            SecretKind::KeyValue,
            "{k}"
        );
    }
    // пути и короткие числа — не секреты
    for t in [
        "password=/etc/ssl/private.pem",
        "token: ~/.config/tok",
        "token=100000",
        "pwd=12345678901",
    ] {
        assert!(scan_secrets(t).is_empty(), "{t}");
    }
    // длинные числа — по-прежнему значение
    assert_eq!(one("password=123456789012"), SecretKind::KeyValue);
}

#[test]
fn private_key_block_whole() {
    let body = rnd(64, 30);
    let t = format!(
        "до\n-----BEGIN OPENSSH PRIVATE KEY-----\n{body}\n{body}\n-----END OPENSSH PRIVATE KEY-----\nпосле"
    );
    let hits = scan_secrets(&t);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].kind, SecretKind::PrivateKey);
    assert_eq!(hits[0].line, 2);
    assert_eq!(mask_secrets(&t, &hits), format!("до\n{SECRET_MARK}\nпосле"));
    // обрезанный блок без END: заголовок и строки base64, текст после остаётся
    let t2 = format!("-----BEGIN PRIVATE KEY-----\n{body}\n\nобычный текст");
    assert_eq!(mask_text(&t2), format!("{SECRET_MARK}\n\nобычный текст"));
}

#[test]
fn lines_and_order() {
    let a = format!("gh{}_{}", "p", rnd(36, 40));
    let t = format!("a\nb {a}\n\npassword=abcdefgh\n");
    let hits = scan_secrets(&t);
    assert_eq!(hits.iter().map(|h| h.line).collect::<Vec<_>>(), [2, 4]);
    assert!(hits[0].range.end <= hits[1].range.start);
    // повторная маскировка ничего не находит
    assert!(scan_secrets(&mask_secrets(&t, &hits)).is_empty());
}

/// Имя перед замаскированным значением (`<имя>=<значение>`, значение — по правилу
/// ключ-значение) проверяется в первом же проходе: повторная маскировка ничего не меняет
/// (находка proptest `mask_is_idempotent`, финальная волна M3).
#[test]
fn name_before_masked_value_idempotent() {
    for t in [
        "0aaaaA0AabA010B3TC1EFG42HebcfIgDpassword=abcdef123".to_string(),
        format!("x {}password=abcdef123 y", rnd(40, 41)),
        "short=1 password=abcdef123".to_string(),
    ] {
        let once = mask_text(&t);
        assert_eq!(mask_text(&once), once, "{t}");
    }
    // короткое имя не трогается: только значение
    assert_eq!(
        mask_text("password=abcdef123"),
        format!("password={SECRET_MARK}")
    );
}

/// Плата за идемпотентность (ревью финальной волны, M-5): имя слева от замаскированного
/// значения проверяется правилом энтропии в первом же проходе. Типичные длинные имена
/// (`UPPER_SNAKE_CASE`, CamelCase без цифр) не маскируются — у них нет строчных или цифр, —
/// а длинное имя со всеми классами символов и высокой энтропией маскируется (раньше это делал
/// второй проход).
#[test]
fn long_name_left_of_masked_value() {
    for name in [
        "DATABASE_CONNECTION_POOL_PRIMARY_PASSWORD",
        "SERVICE_ACCOUNT_2_FOR_REGION_EU_1_PASSWORD",
        "MyVeryLongServiceAccountConfigurationPassword",
    ] {
        let t = format!("{name}=hunter2secret");
        assert_eq!(mask_text(&t), format!("{name}={SECRET_MARK}"), "{name}");
    }
    let random = format!("{}password", rnd(32, 42));
    assert_eq!(
        mask_text(&format!("{random}=hunter2secret")),
        format!("{SECRET_MARK}{SECRET_MARK}"),
        "имя-случайная строка маскируется вместе с разделителем"
    );
}

#[test]
fn overlapping_rules_merge() {
    // токен ZeroAIMemory в значении `token=`: один хит, значение маскируется целиком
    let zam = format!("za{}_{}", "m", rnd(40, 50));
    let t = format!("token={zam}");
    let hits = scan_secrets(&t);
    assert_eq!(hits.len(), 1);
    assert_eq!(mask_secrets(&t, &hits), format!("token={SECRET_MARK}"));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    /// Любой текст: без паники, диапазоны упорядочены и на границах UTF-8, вне диапазонов
    /// байты не меняются.
    #[test]
    fn scan_mask_integrity(
        parts in proptest::collection::vec(
            prop_oneof![
                "[ -~]{0,40}",
                "\\PC{0,20}",
                Just("password=abcdef123".to_string()),
                Just("-----BEGIN PRIVATE KEY-----".to_string()),
                Just("\n".to_string()),
                "[A-Za-z0-9]{32,60}",
                Just("postgres://u:pw12345@h/".to_string()),
            ],
            0..12,
        )
    ) {
        let text = parts.concat();
        let hits = scan_secrets(&text);
        let mut pos = 0;
        let mut masked_expected = String::new();
        for h in &hits {
            prop_assert!(h.range.start >= pos && h.range.end > h.range.start);
            prop_assert!(h.range.end <= text.len());
            prop_assert!(text.is_char_boundary(h.range.start) && text.is_char_boundary(h.range.end));
            prop_assert_eq!(h.line, 1 + text[..h.range.start].matches('\n').count());
            masked_expected.push_str(&text[pos..h.range.start]);
            masked_expected.push_str(SECRET_MARK);
            pos = h.range.end;
        }
        masked_expected.push_str(&text[pos..]);
        prop_assert_eq!(mask_secrets(&text, &hits), masked_expected);
    }

    /// Повторная маскировка ничего не меняет.
    #[test]
    fn mask_is_idempotent(
        parts in proptest::collection::vec(
            prop_oneof![
                "[ -~]{0,40}",
                "\\PC{0,20}",
                Just("password=abcdef123".to_string()),
                Just("token: \"two words here\"".to_string()),
                Just("-----BEGIN PRIVATE KEY-----".to_string()),
                Just("
".to_string()),
                "[A-Za-z0-9=]{32,60}",
                Just("postgres://u:pw12345@h/".to_string()),
                Just(SECRET_MARK.to_string()),
                Just(format!("hf_{}", rnd(34, 40))),
                Just(format!("SG.{}.{}", rnd(22, 41), rnd(43, 42))),
                Just(format!("npm_{}", rnd(36, 43))),
                // ревью b854b22, I2: маска одного правила меняет контекст другого (URL-пароль
                // после маски энтропии или закрытого ключа, ключи после буквы)
                "[A-Za-z0-9=/,:@\"]{1,10}",
                "[A-Za-z0-9/]{32,60}",
                Just("s://".to_string()),
                Just(":hunter22@".to_string()),
                Just(",".to_string()),
                Just("A".to_string()),
                Just(format!("gh{}_{}", "p", rnd(36, 44))),
                Just(format!("{}{}", "AK", "IA") + &rnd(16, 45).to_ascii_uppercase()),
                Just(format!("ey{}{}.{}.{}", "J", rnd(20, 46), rnd(30, 47), rnd(20, 48))),
                Just(format!("xo{}-{}", "xb", rnd(20, 49))),
                // ревью задачи 15b, I1: значение в обратных кавычках
                Just("password=`hunter2hunter2`".to_string()),
                Just("`".to_string()),
            ],
            0..14,
        )
    ) {
        let text = parts.concat();
        let once = mask_text(&text);
        prop_assert_eq!(mask_text(&once), once);
    }
}

/// Бюджет: не больше 2 мс на 100 КБ (только release). `cargo test -p zam-secrets --release
/// --test secrets -- --ignored bench_scan_100kb --nocapture`.
#[test]
#[ignore]
fn bench_scan_100kb() {
    let mut text = String::new();
    let mut i = 0u64;
    while text.len() < 100 * 1024 {
        text.push_str(&format!(
            "Заметка {i}: коммит 9fceb02d0ae598e95dc970b74767f19372d61af8, см. https://example.com/a/b?c=d&e=f \
             password=<your-password> и немного обычного текста про архитектуру памяти, id {}.\n",
            zam_core::Id::new()
        ));
        i += 1;
    }
    scan_secrets(&text); // прогрев ленивых регулярных выражений
    let n = 20;
    let t = std::time::Instant::now();
    for _ in 0..n {
        std::hint::black_box(scan_secrets(&text));
    }
    let per = t.elapsed() / n;
    println!("scan_secrets: {per:?} на {} байт", text.len());
    assert!(per.as_millis() <= 2, "бюджет 2 мс превышен: {per:?}");
}

/// Регрессия (CI 41605984321): `://` замаскированного значения не должен скрывать длинное имя
/// слева только в первом проходе.
#[test]
fn name_left_of_masked_url_value_idempotent() {
    let name = "a020abAb0bC0e13h2cA45aaf6cgDEi1B";
    for tail in [
        "password=abcdef123postgres://u:pw12345@h/",
        "password=abcdef123password=abcdef123postgres://u:pw12345@h/",
    ] {
        let t = format!("{name}{tail}");
        let once = mask_text(&t);
        assert_eq!(mask_text(&once), once, "{t}");
    }
}

/// Полный прогон перед слиянием M5.6 (`mask_is_idempotent`, seed `cc ef3a6e…`): ключ вплотную
/// после буквы (`Ahf_<34>`, граница слова правила префикса не срабатывает — ловит правило
/// энтропии), дальше в том же URL-токене — `password=…postgres://…`. Первый проход видел `://`
/// ещё не замаскированного значения и отбрасывал кандидата как часть URL, второй — уже нет:
/// ключ уходил открытым. Правило энтропии смотрит на текст, каким его увидит повторный проход.
#[test]
fn key_glued_after_letter_before_masked_url_value() {
    let tail = format!("{SECRET_MARK}password=abcdef123postgres://u:pw12345@h/");
    let keys = [
        // тело — как у генератора `mask_is_idempotent` (`rnd(34, 40)`)
        format!("{}_{}", "hf", rnd(34, 40)),
        format!("{}_{}", "hf", rnd(34, 70)),
        format!("gh{}_{}", "p", rnd(36, 71)),
        format!("np{}_{}", "m", rnd(36, 72)),
        format!("za{}_{}", "m", rnd(40, 73)),
        format!("AI{}{}", "za", rnd(35, 74)),
        format!("s{}-{}", "k", rnd(40, 75)),
        format!("xo{}-{}", "xb", rnd(40, 76)),
        format!("gl{}-{}", "pat", rnd(40, 77)),
        format!("github{}pat_{}", "_", rnd(70, 78)),
        format!("ey{}{}.{}.{}", "J", rnd(20, 79), rnd(30, 80), rnd(20, 81)),
        format!("{}.{}.{}", "SG", rnd(22, 82), rnd(43, 83)),
        format!("{}{}", "AK", "IA") + &rnd(16, 84).to_ascii_uppercase(),
    ];
    for lead in ["A", "7", "x "] {
        for key in &keys {
            for t in [
                format!("{lead}{key}{tail}"),
                format!("{lead}{key} {tail}"),
                format!("{lead}{key}{tail} x"),
            ] {
                let once = mask_text(&t);
                assert_eq!(mask_text(&once), once, "{t}");
            }
        }
    }
    // исходный вход: ключ маскируется с первого прохода
    let key = &keys[0];
    let once = mask_text(&format!("A{key}{tail}"));
    assert!(!once.contains(&key[3..]), "{once}");
    assert_eq!(
        once,
        format!("{SECRET_MARK}{SECRET_MARK}password={SECRET_MARK}")
    );
}

/// Ревью b854b22, I1: правило энтропии по виду с масками видит только обрывки между ними —
/// находки по исходному тексту остаются (base64 с `AKIA…` внутри, хвост за `hf_<34>`).
#[test]
fn entropy_over_text_kept_with_masks() {
    let aws = format!("{}{}{}", "AK", "IA", rnd(16, 23).to_ascii_uppercase());
    let b64 = format!("{}+{aws}/{}", rnd(14, 22), rnd(14, 24));
    let hf = format!("{}_{}{}", "hf", rnd(34, 20), rnd(12, 21));
    for v in [b64, hf] {
        let t = format!("k {v} y");
        let once = mask_text(&t);
        assert_eq!(once, format!("k {SECRET_MARK} y"), "{t}");
        assert_eq!(mask_text(&once), once, "{t}");
    }
}

/// Ревью b854b22, I2: маска энтропии или закрытого ключа убирает из имени пользователя URL
/// `/`, `"`, `,` — пароль находится в первом же проходе (скан до неподвижной точки).
#[test]
fn url_password_after_other_mask_first_pass() {
    let cases = [
        (format!("s://x,Ab/{}:hunter22@h", rnd(40, 1)), "hunter22"),
        (
            format!("redis://x\"Q/{}:Sup3rPass@host", rnd(40, 2)),
            "Sup3rPass",
        ),
        (
            "s://-----BEGIN PRIVATE KEY-----:hunter22@".to_string(),
            "hunter22",
        ),
    ];
    for (t, pw) in cases {
        let once = mask_text(&t);
        assert!(!once.contains(pw), "{t} -> {once}");
        assert_eq!(mask_text(&once), once, "{t}");
    }
}

/// Ревью b854b22, M1: цепочка `<имя>=<значение> ` (текст вложений) — линейно: проверки имени
/// перед маской перебирают только находки текущего кандидата.
#[test]
fn name_value_chain_linear() {
    let unit = format!("{}={} ", rnd(10, 9), rnd(40, 10));
    let time = |mb: usize| {
        let t = unit.repeat(mb * 1_000_000 / unit.len());
        let t0 = std::time::Instant::now();
        let hits = scan_secrets(&t);
        let el = t0.elapsed();
        assert_eq!(hits.len(), t.len() / unit.len());
        el
    };
    time(1); // прогрев ленивых регулярных выражений
    let (t1, t2) = (time(1), time(2));
    eprintln!("цепочка имя=значение: 1 МБ {t1:?}, 2 МБ {t2:?}");
    // квадратичная (до правки) — ×4 и секунды в debug; линейная — ×2
    assert!(
        t2 < t1 * 3 + std::time::Duration::from_millis(300),
        "{t1:?} / {t2:?}"
    );
}

/// Ревью задачи 9A, M2: ключи вплотную друг к другу (`SG.…hf_…`, `ghp_…ghp_…`, `AKIA…npm_…`)
/// — находки сканера (детекторы, нарезка) покрывают оба ключа целиком, маскировка — одним
/// проходом. Пересекающиеся находки (правило энтропии) сливаются в одну.
#[test]
fn glued_keys_each_detected() {
    let up = rnd(16, 50).to_ascii_uppercase();
    let cases = [
        (
            format!("SG.{}.{}", rnd(22, 51), rnd(43, 52)),
            format!("hf_{}", rnd(34, 53)),
        ),
        (
            format!("ghp_{}", rnd(36, 54)),
            format!("ghp_{}", rnd(36, 55)),
        ),
        (format!("AKIA{up}"), format!("npm_{}", rnd(36, 56))),
        // второй ключ — только правило префикса (нижний регистр без цифр — не энтропия)
        (
            format!("AKIA{up}"),
            format!("hf_{}", &"abcdefghij".repeat(4)[..34]),
        ),
    ];
    for (a, b) in cases {
        let t = format!("x {a}{b} y");
        let hits = scan_secrets(&t);
        let covered = |at: usize| hits.iter().any(|h| h.range.contains(&at));
        assert!((2..2 + a.len() + b.len()).all(covered), "{t}: {hits:?}");
        let once = mask_text(&t);
        assert!(!once.contains(&a) && !once.contains(&b[3..]), "{once}");
        assert_eq!(mask_text(&once), once, "{t}");
    }
}

/// Повторное ревью 9A: жадный хвост правила (Slack `xox…-[A-Za-z0-9-]{10,}`) съедает начало
/// следующего ключа (`hf`) — ключ всё равно находится (тело без энтропии: ловит только правило
/// префикса); JWT вплотную за ключом (`ghp_<36>eyJ…`) — тоже, одной маской: кандидат энтропии
/// по исходному тексту покрывает оба (ревью b854b22, I1).
#[test]
fn glued_after_greedy_tail_and_jwt() {
    let low = "abcdefghij".repeat(4);
    let cases = [
        (
            format!("xo{}-{}", "xb", "1234567890"),
            format!("hf_{}", &low[..34]),
        ),
        (
            format!("xo{}-{}", "xp", "abcdefghijklmn"),
            format!("npm_{}", &low[..36]),
        ),
        (
            format!("ghp_{}", rnd(36, 60)),
            format!("ey{}{}.{}.{}", "J", rnd(20, 61), rnd(30, 62), rnd(20, 63)),
        ),
    ];
    for (a, b) in cases {
        let t = format!("x {a}{b} y");
        let hits = scan_secrets(&t);
        let covered = |at: usize| hits.iter().any(|h| h.range.contains(&at));
        assert!((2..2 + a.len() + b.len()).all(covered), "{t}: {hits:?}");
        let once = mask_text(&t);
        assert_eq!(once, format!("x {SECRET_MARK} y"), "{t}");
        assert_eq!(mask_text(&once), once);
    }
}

/// Повторное ревью 9A: цепочка слипшихся ключей не квадратична — 1 МБ из `AKIA<16>` подряд
/// (≈ 52 000 ключей) сканируется быстро (замер — в журнале этапа).
#[test]
fn glued_chain_1mb_linear() {
    let key = format!("{}{}{}", "AK", "IA", "ABCDEFGH23456789");
    let t = key.repeat(1_048_576 / key.len());
    let t0 = std::time::Instant::now();
    let hits = scan_secrets(&t);
    let ms = t0.elapsed().as_millis();
    eprintln!("1 МБ AKIA-цепочки: {} находок за {ms} мс", hits.len());
    assert_eq!(hits.len(), t.len() / key.len());
    assert!(ms < 5000, "{ms} мс");
}

/// Ревью задачи 15b M6, I1: значение в обратных кавычках после ключа маскируется (кавычки
/// остаются); обычный inline code без ключа — нет.
#[test]
fn backtick_value_masked() {
    let pw = format!("hunter2{}", "hunter2");
    let cases = [
        (
            format!("password=`{pw}`"),
            format!("password=`{SECRET_MARK}`"),
        ),
        (
            format!("password: `{pw}`"),
            format!("password: `{SECRET_MARK}`"),
        ),
        (
            format!("DB_PASSWORD=`Sup3r{}`", "S3cret!"),
            format!("DB_PASSWORD=`{SECRET_MARK}`"),
        ),
        (
            format!("token = `abcdef{}`", "123456"),
            format!("token = `{SECRET_MARK}`"),
        ),
        // пароль-фраза с пробелами — до закрывающей кавычки
        (
            "secret: `two words here` и дальше".to_string(),
            format!("secret: `{SECRET_MARK}` и дальше"),
        ),
    ];
    for (t, want) in &cases {
        assert_eq!(one(t), SecretKind::KeyValue, "{t}");
        assert_eq!(&mask_text(t), want, "{t}");
        assert_eq!(mask_text(want), *want, "идемпотентно: {t}");
    }
    for t in [
        "запустите `cargo build` и `cargo test --release`",
        "`password` — имя поля, а не значение",
        "password=`${DB_PASSWORD}`",
        "token: `<your-token>`",
        "password=`short`",
    ] {
        assert!(scan_secrets(t).is_empty(), "{t}: {:?}", scan_secrets(t));
    }
}
