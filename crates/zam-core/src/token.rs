//! Токены доступа: генерация, хэш, маска.

use rand::Rng;
use rand::TryRngCore;
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};

const PREFIX: &str = "zam_";
const SECRET_LEN: usize = 40;
const DISPLAY_PREFIX_LEN: usize = 8;
const BASE62: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// Свежесозданный токен: открытый текст показывается один раз, в БД — только хэш.
#[derive(Clone)]
pub struct NewToken {
    pub plaintext: String,
    pub prefix: String,
    pub secret_hash: String,
}

/// В Debug открытый текст — только маска, хэш не выводится.
impl std::fmt::Debug for NewToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewToken")
            .field("plaintext", &mask_token(&self.plaintext))
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

/// Генерирует токен `zam_` + 40 символов base62 из ОС-источника случайности.
pub fn generate_token() -> NewToken {
    let mut rng = OsRng.unwrap_err();
    let secret: String = (0..SECRET_LEN)
        // random_range даёт равномерное распределение без смещения
        .map(|_| BASE62[rng.random_range(0..BASE62.len())] as char)
        .collect();
    let plaintext = format!("{PREFIX}{secret}");
    NewToken {
        prefix: secret[..DISPLAY_PREFIX_LEN].to_string(),
        secret_hash: hash_token(&plaintext),
        plaintext,
    }
}

/// sha256 всей строки токена, hex в нижнем регистре.
pub fn hash_token(plaintext: &str) -> String {
    use std::fmt::Write;
    Sha256::digest(plaintext.as_bytes())
        .iter()
        .fold(String::with_capacity(64), |mut s, b| {
            write!(s, "{b:02x}").expect("запись в String");
            s
        })
}

/// Соответствует ли строка формату токена.
pub fn looks_like_token(s: &str) -> bool {
    s.len() == PREFIX.len() + SECRET_LEN
        && s.starts_with(PREFIX)
        && s.as_bytes()[PREFIX.len()..]
            .iter()
            .all(u8::is_ascii_alphanumeric)
}

/// Маска для логов и UI: `zam_` + префикс + `…`; не токен — `***`.
pub fn mask_token(s: &str) -> String {
    if looks_like_token(s) {
        let start = PREFIX.len();
        format!("{PREFIX}{}…", &s[start..start + DISPLAY_PREFIX_LEN])
    } else {
        "***".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_format_and_hash() {
        let t = generate_token();
        assert!(t.plaintext.starts_with("zam_") && t.plaintext.len() == 44);
        assert!(t.plaintext[4..].chars().all(|c| c.is_ascii_alphanumeric()));
        assert_eq!(t.prefix, &t.plaintext[4..12]);
        assert_eq!(t.secret_hash, hash_token(&t.plaintext));
        assert_eq!(t.secret_hash.len(), 64);
        assert!(
            t.secret_hash
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        );
        assert_ne!(generate_token().plaintext, t.plaintext);
    }
    #[test]
    fn hash_known_vector() {
        assert_eq!(
            hash_token("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
    #[test]
    fn mask_never_leaks_secret() {
        let t = generate_token();
        let m = mask_token(&t.plaintext);
        assert_eq!(m, format!("zam_{}…", t.prefix));
        assert!(!m.contains(&t.plaintext[12..]));
        assert_eq!(mask_token("garbage"), "***");
    }
    #[test]
    fn debug_masks_plaintext() {
        let t = generate_token();
        let d = format!("{t:?}");
        assert!(!d.contains(&t.plaintext), "{d}");
        assert!(d.contains(&mask_token(&t.plaintext)), "{d}");
        assert!(d.contains(&t.prefix), "{d}");
    }
    #[test]
    fn looks_like_token_checks_format() {
        assert!(looks_like_token(&generate_token().plaintext));
        assert!(!looks_like_token("zam_short"));
        assert!(!looks_like_token(&format!("zam_{}", "-".repeat(40))));
        assert!(!looks_like_token(&format!("xxx_{}", "a".repeat(40))));
        assert!(!looks_like_token(&format!("zam_{}é", "a".repeat(39))));
    }
}
