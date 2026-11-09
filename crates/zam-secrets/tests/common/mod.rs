//! Общие данные тестов сканера секретов: генератор `rnd`, корпус входов тестов сканера
//! ([`corpus`]), атомы генератора ([`atoms`]) и закреплённый вход упора в `FIXPOINT_MAX`
//! ([`depth_capped_input`]).
//!
//! Модуль подключают тесты `zam-secrets` (`mod common;`) и тест реэкспорта `zam-markdown`
//! (`#[path = "../../zam-secrets/tests/common/mod.rs"]`), поэтому зависимостей, кроме `std`, у
//! него нет.
//!
//! Тестовые «секреты» собираются из частей во время выполнения: в исходниках нет строк,
//! похожих на настоящие ключи (сканеры утечек репозитория, GitHub push protection).

#![allow(dead_code)]

/// Метка маски — как `zam_secrets::SECRET_MARK` (модуль без зависимостей; равенство проверяет
/// тест `secrets_reexport_identical`).
pub const MARK: &str = "‹secret›";

/// Псевдослучайная строка base62 заданной длины (детерминированная, с высокой энтропией).
pub fn rnd(len: usize, seed: u64) -> String {
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

/// Ключи всех семейств правила префикса и JWT (собраны из частей).
pub fn keys() -> Vec<String> {
    vec![
        format!("{}{}", "AK", "IA") + &rnd(16, 1).to_ascii_uppercase(),
        format!("{}{}", "AS", "IA") + &rnd(16, 2).to_ascii_uppercase(),
        format!("gh{}_{}", "p", rnd(36, 3)),
        format!("github{}pat_{}", "_", rnd(70, 4)),
        format!("gl{}-{}", "pat", rnd(24, 5)),
        format!("xo{}-{}", "xb", "1234567890-abcdefghij"),
        format!("s{}-{}", "k", rnd(30, 6)),
        format!("s{}-ant-{}", "k", rnd(30, 7)),
        format!("AI{}{}", "za", rnd(35, 8)),
        format!("{}_{}", "hf", rnd(34, 30)),
        format!("np{}_{}", "m", rnd(36, 31)),
        format!("{}.{}.{}", "SG", rnd(22, 32), rnd(43, 33)),
        format!("za{}_{}", "m", rnd(40, 9)),
        format!("ey{}{}.{}.{}", "J", rnd(20, 10), rnd(30, 11), rnd(20, 12)),
    ]
}

/// Корпус входов тестов сканера: секреты каждого правила, идентификаторы без находок, слипшиеся
/// ключи, имена перед масками, URL-пароли за масками, закрытые ключи, обычный текст заметки.
pub fn corpus() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for k in keys() {
        out.push(format!("key {k} end"));
        out.push(format!("token={k}"));
        out.push(format!(
            "A{k}{MARK}password=abcdef123postgres://u:pw12345@h/"
        ));
        out.push(format!("x {k}{} y", keys()[9]));
    }
    let pk_body = rnd(64, 13);
    out.extend([
        format!("-----BEGIN {} PRIVATE KEY-----\n{pk_body}\n-----END {} PRIVATE KEY-----", "RSA", "RSA"),
        format!("-----BEGIN PRIVATE KEY-----\n{pk_body}\n\nобычный текст"),
        "password = hunter2hunter2".into(),
        "api_key: abcdef123456".into(),
        r#"{"client_secret": "s3cr3t-value"}"#.into(),
        "postgres://admin:s3cr3tpw@db.example.org:5432/app".into(),
        format!("value {} here", rnd(40, 14)),
        "commit 9fceb02d0ae598e95dc970b74767f19372d61af8 fixes".into(),
        "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into(),
        "id 550e8400-e29b-41d4-a716-446655440000".into(),
        "ссылка https://example.com/search/results/page?q=rust+regex&utm_source=newsletter&ref=abcdefghijklmnopqrstuvwxyz0123456789ABCD".into(),
        format!("integrity sha512-{}", rnd(86, 20)),
        "password=<your-password>".into(),
        "token: ${TOKEN}".into(),
        "password: $DB_PASSWORD".into(),
        "src/components/UserProfileCard/Details2/ProfileCardDetailsView.tsx".into(),
        "обычный текст про токены и пароли: без значений".into(),
        "config:\n  password = hunter2hunter2 # prod\nnext line".into(),
        r#"db_password: "p@ssw0rd-123""#.into(),
        "redis://user:topsecret99@host:6379/0".into(),
        "redis://:pw123456@host:6379".into(),
        r#"password: "correct horse battery" ok"#.into(),
        "password=/etc/ssl/private.pem".into(),
        "password=123456789012".into(),
        format!(r#"{{"url":"https://a.example/c","k":"{}"}}"#, rnd(40, 60)),
        format!("(https://a.example/{}/x)", rnd(40, 61)),
        "0aaaaA0AabA010B3TC1EFG42HebcfIgDpassword=abcdef123".into(),
        format!("x {}password=abcdef123 y", rnd(40, 41)),
        "DATABASE_CONNECTION_POOL_PRIMARY_PASSWORD=hunter2secret".into(),
        format!("{}password=hunter2secret", rnd(32, 42)),
        "a020abAb0bC0e13h2cA45aaf6cgDEi1Bpassword=abcdef123password=abcdef123postgres://u:pw12345@h/".into(),
        format!("s://x,Ab/{}:hunter22@h", rnd(40, 1)),
        format!("redis://x\"Q/{}:Sup3rPass@host", rnd(40, 2)),
        "s://-----BEGIN PRIVATE KEY-----:hunter22@".into(),
        format!("k {}+{}{}{}/{} y", rnd(14, 22), "AK", "IA", rnd(16, 23).to_ascii_uppercase(), rnd(14, 24)),
        format!("{}={} {}={} ", rnd(10, 9), rnd(40, 10), rnd(10, 11), rnd(40, 12)),
        format!("x {}{}{} y", "xo", "xb-1234567890", concat!("hf", "_abcdefghijabcdefghijabcdefghijabcd")),
        format!("{MARK}{MARK} уже замаскировано"),
        String::new(),
        note_text(),
    ]);
    out
}

/// Обычная заметка: коммиты, ссылки, идентификаторы, заглушки паролей, один ключ и один пароль.
pub fn note_text() -> String {
    let mut t = String::new();
    for i in 0..50 {
        t.push_str(&format!(
            "Заметка {i}: коммит 9fceb02d0ae598e95dc970b74767f19372d61af8, см. https://example.com/a/b?c=d&e=f \
             password=<your-password> и немного текста про архитектуру памяти.\n"
        ));
    }
    t.push_str(&format!(
        "GITHUB_TOKEN=gh{}_{}\npassword = hunter2hunter2\n",
        "p",
        rnd(36, 90)
    ));
    t
}

/// Атомы генератора входов (как у proptest `mask_is_idempotent`) в виде шаблонов [`expand`]:
/// ключи, маски, фрагменты URL, `имя=значение`, случайные строки с `/`, `=`, `,`, `"`, `:`, `@`.
pub fn atoms() -> Vec<String> {
    let mut a: Vec<String> = [
        "password=abcdef123",
        "token: \"two words here\"",
        "-----BEGIN PRIVATE KEY-----",
        "{nl}",
        "postgres://u:pw12345@h/",
        "{mark}",
        "s://",
        ":hunter22@",
        ",",
        "A",
        "/",
        "\"",
        "=",
        " ",
        "@",
        ":",
    ]
    .map(String::from)
    .into();
    // фрагменты URL-пароля и `имя=` без значения: каскады «маска → новая находка»
    a.extend(
        [
            "t://",
            "s://,",
            ".",
            "password=ab",
            ":\"pw@",
            "password=",
            "'",
            "ab:",
            "x://y",
            "@h/",
        ]
        .map(String::from),
    );
    a.push("pwd=".into());
    a.push("`".into());
    a.extend((0..keys().len()).map(|i| format!("{{key:{i}}}")));
    for s in 0..6 {
        a.push(format!("{{rnd:40:{}}}", 100 + s));
        a.push(format!("{{rnd:20:{}}}/{{rnd:20:{}}}", 200 + s, 300 + s));
        a.push(format!("{{rnd:6:{}}}", 400 + s));
    }
    a
}

/// Раскрывает шаблон: `{nl}` — перевод строки, `{mark}` — [`MARK`], `{key:I}` — `keys()[I]`,
/// `{rnd:LEN:SEED}` — [`rnd`]; остальное — как есть.
pub fn expand(t: &str) -> String {
    let keys = keys();
    let mut out = String::new();
    let mut rest = t;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let j = rest[i..].find('}').expect("незакрытый шаблон") + i;
        let tok: Vec<&str> = rest[i + 1..j].split(':').collect();
        match tok.as_slice() {
            ["nl"] => out.push('\n'),
            ["mark"] => out.push_str(MARK),
            ["key", n] => out.push_str(&keys[n.parse::<usize>().unwrap()]),
            ["rnd", l, s] => out.push_str(&rnd(l.parse().unwrap(), s.parse().unwrap())),
            _ => panic!("неизвестный шаблон {:?}", &rest[i..=j]),
        }
        rest = &rest[j + 1..];
    }
    out.push_str(rest);
    out
}

/// Детерминированный выбор `n` атомов по зерну `seed` (xorshift): шаблон (склейка атомов).
pub fn combo(atoms: &[String], seed: u64, n: usize) -> String {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut t = String::new();
    for _ in 0..n {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        t.push_str(&atoms[(x % atoms.len() as u64) as usize]);
    }
    t
}
