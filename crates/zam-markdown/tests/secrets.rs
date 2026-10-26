//! Сканер секретов §7.6: правила, отсутствие ложных срабатываний на идентификаторах,
//! маскирование только значения, property-тест целостности текста.
//!
//! Тестовые «секреты» собираются из частей во время выполнения: в исходниках нет строк,
//! похожих на настоящие ключи (сканеры утечек репозитория).

use proptest::prelude::*;
use zam_markdown::{SECRET_MARK, SecretKind, mask_secrets, mask_text, scan_secrets};

/// Псевдослучайная строка base62 заданной длины (детерминированная, с высокой энтропией).
fn rnd(len: usize, seed: u64) -> String {
    const A: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let mut x = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    (0..len)
        .map(|_| {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            A[((x >> 33) % 62) as usize] as char
        })
        .collect()
}

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
            ],
            0..12,
        )
    ) {
        let text = parts.concat();
        let once = mask_text(&text);
        prop_assert_eq!(mask_text(&once), once);
    }
}

/// Бюджет: не больше 2 мс на 100 КБ (только release). `cargo test -p zam-markdown --release
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
