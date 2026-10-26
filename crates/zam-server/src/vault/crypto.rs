//! Шифрование Vault (§7.6): XChaCha20-Poly1305 (RustCrypto `chacha20poly1305`).
//!
//! - Блоб: `0x01 ‖ nonce(24) ‖ ciphertext‖tag(16)`; nonce — 24 случайных байта ОС на каждое
//!   шифрование (у XChaCha20 случайный nonce безопасен без счётчика).
//! - AAD значения: `b"zam-vault-v1" ‖ user_id ‖ item_id ‖ rev` — id в тексте (26 символов
//!   Crockford base32, фиксированная длина), `rev` — 8 байт big-endian. Шифротекст,
//!   перенесённый в другую запись, ревизию или базу другого пользователя, не расшифровывается.
//! - Ключ пользователя (32 байта) оборачивается мастер-ключом с AAD `b"zam-ukey-v1" ‖ user_id`.
//! - Проверочный шифротекст мастер-ключа (`vault.key_check`) — `b"zam-key-check"` с AAD
//!   `b"zam-key-check"`.
//!
//! Ключи и расшифрованные значения — в [`Zeroizing`]: память затирается при освобождении.
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use rand::TryRngCore;
use rand::rngs::OsRng;
use zam_core::Id;
use zeroize::Zeroizing;

/// Версия формата блоба.
pub const BLOB_V1: u8 = 0x01;
/// Длина nonce XChaCha20, байт.
pub const NONCE_LEN: usize = 24;
/// Длина тега Poly1305, байт.
pub const TAG_LEN: usize = 16;
/// Длина ключа, байт.
pub const KEY_LEN: usize = 32;
/// Открытый текст проверочного шифротекста мастер-ключа.
pub const KEY_CHECK_PLAIN: &[u8] = b"zam-key-check";
/// AAD проверочного шифротекста.
pub const KEY_CHECK_AAD: &[u8] = b"zam-key-check";

/// 32-байтный ключ, затираемый при освобождении.
pub type SecretKey = Zeroizing<[u8; KEY_LEN]>;

/// Ошибка расшифровки (подробности не раскрываются: ни ключ, ни текст в неё не входят).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CryptoError {
    #[error("неизвестный формат шифротекста Vault")]
    Format,
    #[error("шифротекст Vault не проходит проверку (другой ключ, запись или ревизия)")]
    Auth,
}

/// Случайные байты ОС; без источника случайности работать нельзя.
fn fill_random(buf: &mut [u8]) {
    OsRng.try_fill_bytes(buf).expect("источник случайности ОС");
}

/// Новый случайный ключ.
pub fn random_key() -> SecretKey {
    let mut k = Zeroizing::new([0u8; KEY_LEN]);
    fill_random(k.as_mut());
    k
}

/// Ключ из байтов; длина не 32 — `None`.
pub fn key_from(bytes: &[u8]) -> Option<SecretKey> {
    let arr: [u8; KEY_LEN] = bytes.try_into().ok()?;
    Some(Zeroizing::new(arr))
}

/// AAD значения записи `item` ревизии `rev` пользователя `user`.
pub fn item_aad(user: Id, item: Id, rev: i64) -> Vec<u8> {
    let mut a = b"zam-vault-v1".to_vec();
    a.extend_from_slice(user.to_string().as_bytes());
    a.extend_from_slice(item.to_string().as_bytes());
    a.extend_from_slice(&rev.to_be_bytes());
    a
}

/// AAD обёрнутого ключа пользователя `user`.
pub fn user_key_aad(user: Id) -> Vec<u8> {
    let mut a = b"zam-ukey-v1".to_vec();
    a.extend_from_slice(user.to_string().as_bytes());
    a
}

/// Шифрует `plain` ключом `key` с `aad` → блоб формата v1.
pub fn seal(key: &[u8; KEY_LEN], aad: &[u8], plain: &[u8]) -> Vec<u8> {
    let cipher = XChaCha20Poly1305::new(Key::from_slice(key));
    let mut nonce = [0u8; NONCE_LEN];
    fill_random(&mut nonce);
    let ct = cipher
        .encrypt(XNonce::from_slice(&nonce), Payload { msg: plain, aad })
        .expect("XChaCha20-Poly1305: шифрование не ограничено по длине для Vault");
    let mut blob = Vec::with_capacity(1 + NONCE_LEN + ct.len());
    blob.push(BLOB_V1);
    blob.extend_from_slice(&nonce);
    blob.extend_from_slice(&ct);
    blob
}

/// Расшифровывает блоб v1; другой ключ, AAD или испорченный блоб — ошибка.
pub fn open(
    key: &[u8; KEY_LEN],
    aad: &[u8],
    blob: &[u8],
) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    if blob.len() < 1 + NONCE_LEN + TAG_LEN || blob[0] != BLOB_V1 {
        return Err(CryptoError::Format);
    }
    let (nonce, ct) = blob[1..].split_at(NONCE_LEN);
    let cipher = XChaCha20Poly1305::new(Key::from_slice(key));
    cipher
        .decrypt(XNonce::from_slice(nonce), Payload { msg: ct, aad })
        .map(Zeroizing::new)
        .map_err(|_| CryptoError::Auth)
}

/// Оборачивает ключ пользователя мастер-ключом.
pub fn wrap_user_key(master: &[u8; KEY_LEN], user: Id, key: &[u8; KEY_LEN]) -> Vec<u8> {
    seal(master, &user_key_aad(user), key)
}

/// Разворачивает ключ пользователя.
pub fn unwrap_user_key(
    master: &[u8; KEY_LEN],
    user: Id,
    wrapped: &[u8],
) -> Result<SecretKey, CryptoError> {
    let plain = open(master, &user_key_aad(user), wrapped)?;
    key_from(&plain).ok_or(CryptoError::Format)
}

/// Проверочный шифротекст мастер-ключа.
pub fn key_check(master: &[u8; KEY_LEN]) -> Vec<u8> {
    seal(master, KEY_CHECK_AAD, KEY_CHECK_PLAIN)
}

/// Мастер-ключ подходит к проверочному шифротексту.
pub fn key_check_ok(master: &[u8; KEY_LEN], blob: &[u8]) -> bool {
    open(master, KEY_CHECK_AAD, blob).is_ok_and(|p| p.as_slice() == KEY_CHECK_PLAIN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let k = random_key();
        let (u, i) = (Id::new(), Id::new());
        for plain in [&b""[..], b"s3cret", &[0xffu8; 70_000][..]] {
            let blob = seal(&k, &item_aad(u, i, 1), plain);
            assert_eq!(blob[0], BLOB_V1);
            assert_eq!(blob.len(), 1 + NONCE_LEN + plain.len() + TAG_LEN);
            assert!(
                plain.len() < 8 || !blob.windows(plain.len()).any(|w| w == plain),
                "открытый текст в блобе"
            );
            let back = open(&k, &item_aad(u, i, 1), &blob).unwrap();
            assert_eq!(back.as_slice(), plain);
        }
        // nonce случайный: два шифрования одного текста различаются
        let a = seal(&k, b"x", b"same");
        let b = seal(&k, b"x", b"same");
        assert_ne!(a, b);
        assert_ne!(a[1..1 + NONCE_LEN], b[1..1 + NONCE_LEN]);
    }

    #[test]
    fn aad_binding_swap_blob_fails() {
        let k = random_key();
        let (u, i) = (Id::new(), Id::new());
        let blob = seal(&k, &item_aad(u, i, 2), b"s3cret");
        // другая запись, ревизия, пользователь, ключ — не расшифровывается
        for aad in [
            item_aad(u, Id::new(), 2),
            item_aad(u, i, 1),
            item_aad(u, i, 3),
            item_aad(Id::new(), i, 2),
        ] {
            assert_eq!(open(&k, &aad, &blob).unwrap_err(), CryptoError::Auth);
        }
        assert_eq!(
            open(&random_key(), &item_aad(u, i, 2), &blob).unwrap_err(),
            CryptoError::Auth
        );
        // порча любого байта шифротекста или nonce
        for pos in [1, NONCE_LEN, blob.len() - 1] {
            let mut bad = blob.clone();
            bad[pos] ^= 1;
            assert!(open(&k, &item_aad(u, i, 2), &bad).is_err(), "байт {pos}");
        }
        // версия и длина
        let mut v2 = blob.clone();
        v2[0] = 0x02;
        assert_eq!(
            open(&k, &item_aad(u, i, 2), &v2).unwrap_err(),
            CryptoError::Format
        );
        assert_eq!(
            open(&k, &item_aad(u, i, 2), &blob[..1 + NONCE_LEN + TAG_LEN - 1]).unwrap_err(),
            CryptoError::Format
        );
        // AAD однозначен: id фиксированной длины, rev — 8 байт
        assert_eq!(item_aad(u, i, 1).len(), 12 + 26 + 26 + 8);
    }

    #[test]
    fn wrapped_user_key_roundtrip() {
        let master = random_key();
        let (u, other) = (Id::new(), Id::new());
        let uk = random_key();
        let w = wrap_user_key(&master, u, &uk);
        assert!(!w.windows(KEY_LEN).any(|x| x == uk.as_slice()));
        assert_eq!(
            unwrap_user_key(&master, u, &w).unwrap().as_slice(),
            uk.as_slice()
        );
        // чужой пользователь или другой мастер-ключ — нет
        assert!(unwrap_user_key(&master, other, &w).is_err());
        assert!(unwrap_user_key(&random_key(), u, &w).is_err());
        // проверочный шифротекст
        let kc = key_check(&master);
        assert!(key_check_ok(&master, &kc));
        assert!(!key_check_ok(&random_key(), &kc));
        assert!(!key_check_ok(
            &master,
            &seal(&master, KEY_CHECK_AAD, b"other")
        ));
    }
}
