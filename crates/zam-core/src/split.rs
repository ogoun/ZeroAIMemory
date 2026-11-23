//! Часть набора проб ночной проверки памяти (M7.3 §6.4 п. 2): `dev` — для калибровки порогов,
//! `test` — для вердикта. Тип общий у хранилища проб (`zam_store::probes`) и функций разбиения
//! (`zam_search::eval::probe_split`): проба записывается с частью, посчитанной поиском, без
//! перевода между двумя одинаковыми перечислениями.

use serde::{Deserialize, Serialize};

/// Часть набора проб.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Split {
    Dev,
    Test,
}

impl Split {
    pub const ALL: [Split; 2] = [Split::Dev, Split::Test];

    /// Значение колонки `probes.split` и поля JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            Split::Dev => "dev",
            Split::Test => "test",
        }
    }

    pub fn parse(s: &str) -> Option<Split> {
        Split::ALL.into_iter().find(|x| x.as_str() == s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_as_str_parse_inverse() {
        for s in Split::ALL {
            assert_eq!(Split::parse(s.as_str()), Some(s));
        }
        assert_eq!(Split::parse("holdout"), None);
        assert_eq!(serde_json::to_string(&Split::Test).unwrap(), "\"test\"");
    }
}
