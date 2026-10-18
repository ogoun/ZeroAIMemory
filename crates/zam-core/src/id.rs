//! Идентификаторы: UUIDv7, наружу — Crockford base32 (26 символов).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Ошибка разбора идентификатора.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseIdError;

impl fmt::Display for ParseIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("некорректный идентификатор")
    }
}

impl std::error::Error for ParseIdError {}

/// Идентификатор сущности (UUIDv7). Сортировка строк base32 совпадает с порядком создания.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Id(Uuid);

impl Id {
    /// Новый идентификатор UUIDv7.
    pub fn new() -> Id {
        Id(Uuid::now_v7())
    }

    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl Default for Id {
    fn default() -> Self {
        Id::new()
    }
}

impl From<Uuid> for Id {
    fn from(u: Uuid) -> Self {
        Id(u)
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Big-endian: старший символ несёт 3 бита, остальные по 5 (как в ULID).
        let v = self.0.as_u128();
        let mut buf = [0u8; 26];
        for (i, b) in buf.iter_mut().enumerate() {
            *b = ALPHABET[((v >> (5 * (25 - i))) & 31) as usize];
        }
        f.write_str(std::str::from_utf8(&buf).expect("алфавит ASCII"))
    }
}

fn decode_char(c: u8) -> Option<u8> {
    let c = c.to_ascii_uppercase();
    match c {
        b'O' => Some(0),
        b'I' | b'L' => Some(1),
        _ => ALPHABET.iter().position(|&a| a == c).map(|p| p as u8),
    }
}

impl FromStr for Id {
    type Err = ParseIdError;

    fn from_str(s: &str) -> Result<Id, ParseIdError> {
        if s.len() == 26 {
            let mut v: u128 = 0;
            for (i, c) in s.bytes().enumerate() {
                let d = decode_char(c).ok_or(ParseIdError)?;
                if i == 0 && d > 7 {
                    return Err(ParseIdError); // не помещается в 128 бит
                }
                v = (v << 5) | u128::from(d);
            }
            return Ok(Id(Uuid::from_u128(v)));
        }
        Uuid::parse_str(s).map(Id).map_err(|_| ParseIdError)
    }
}

impl Serialize for Id {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Id {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Id, D::Error> {
        let s = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(feature = "sqlite")]
impl rusqlite::ToSql for Id {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(self.to_string().into())
    }
}

#[cfg(feature = "sqlite")]
impl rusqlite::types::FromSql for Id {
    fn column_result(v: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        v.as_str()?
            .parse()
            .map_err(|e| rusqlite::types::FromSqlError::Other(Box::new(e)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_display_is_26_chars() {
        assert_eq!(Id::new().to_string().len(), 26);
    }
    #[test]
    fn id_roundtrip_base32_and_uuid() {
        let id = Id::new();
        assert_eq!(id.to_string().parse::<Id>().unwrap(), id);
        assert_eq!(id.to_string().to_lowercase().parse::<Id>().unwrap(), id);
        assert_eq!(id.as_uuid().to_string().parse::<Id>().unwrap(), id);
    }
    #[test]
    fn ids_sort_by_creation_time() {
        let a = Id::new();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = Id::new();
        assert!(a.to_string() < b.to_string());
    }
    #[test]
    fn id_rejects_garbage() {
        assert!("not-an-id".parse::<Id>().is_err());
        assert!("".parse::<Id>().is_err());
        // 26 символов, но старший символ > 7 (переполнение 128 бит)
        assert!("8ZZZZZZZZZZZZZZZZZZZZZZZZZ".parse::<Id>().is_err());
        // недопустимая буква U
        assert!("0UUUUUUUUUUUUUUUUUUUUUUUUU".parse::<Id>().is_err());
    }
    #[test]
    fn id_known_vectors_and_aliases() {
        let nil: Id = "00000000000000000000000000".parse().unwrap();
        assert_eq!(nil.as_uuid().as_u128(), 0);
        let max: Id = "7ZZZZZZZZZZZZZZZZZZZZZZZZZ".parse().unwrap();
        assert_eq!(max.as_uuid().as_u128(), u128::MAX);
        assert_eq!(max.to_string(), "7ZZZZZZZZZZZZZZZZZZZZZZZZZ");
        let a: Id = "0IL0O00000000000000000000O".parse().unwrap();
        let b: Id = "01100000000000000000000000".parse().unwrap();
        assert_eq!(a, b);
    }
    #[test]
    fn id_serde_as_base32_string() {
        let id = Id::new();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{id}\""));
        assert_eq!(serde_json::from_str::<Id>(&json).unwrap(), id);
        assert!(serde_json::from_str::<Id>("\"bad\"").is_err());
    }
    #[cfg(feature = "sqlite")]
    #[test]
    fn id_sqlite_roundtrip() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE t (id TEXT NOT NULL)", [])
            .unwrap();
        let id = Id::new();
        conn.execute("INSERT INTO t (id) VALUES (?1)", [&id])
            .unwrap();
        let (raw, back): (String, Id) = conn
            .query_row("SELECT id, id FROM t", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!(raw, id.to_string());
        assert_eq!(back, id);
    }
}
