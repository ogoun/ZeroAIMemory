//! Идентификатор решения (`agent_solution_id`, §11.6 ТЗ): UUID с дефисами в нижнем регистре.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

use crate::ParseIdError;

/// `agent_solution_id`: UUID с дефисами, хранится в нижнем регистре.
///
/// Разбор принимает только каноническую длину 36 символов (`8-4-4-4-12`) в любом регистре —
/// как `canonical_guid` сервера; формы без дефисов, в скобках и `urn:uuid:` отклоняются.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SolutionGuid(String);

impl SolutionGuid {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Новый GUID (UUIDv7).
    pub fn new_v7() -> SolutionGuid {
        SolutionGuid(Uuid::now_v7().hyphenated().to_string())
    }
}

impl FromStr for SolutionGuid {
    type Err = ParseIdError;

    fn from_str(s: &str) -> Result<SolutionGuid, ParseIdError> {
        if s.len() != 36 {
            return Err(ParseIdError);
        }
        Uuid::try_parse(s)
            .map(|u| SolutionGuid(u.hyphenated().to_string()))
            .map_err(|_| ParseIdError)
    }
}

impl fmt::Display for SolutionGuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for SolutionGuid {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SolutionGuid {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<SolutionGuid, D::Error> {
        let s = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(feature = "sqlite")]
impl rusqlite::ToSql for SolutionGuid {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(self.0.as_str().into())
    }
}

#[cfg(feature = "sqlite")]
impl rusqlite::types::FromSql for SolutionGuid {
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
    fn guid_canonical_forms() {
        let up = "0192F0A1-7C3B-7D2E-9A41-3B5C6D7E8F90";
        let g: SolutionGuid = up.parse().unwrap();
        assert_eq!(g.as_str(), "0192f0a1-7c3b-7d2e-9a41-3b5c6d7e8f90");
        assert_eq!(g.to_string(), g.as_str());
        assert_eq!(g, g.as_str().parse().unwrap());
        // только 36 символов с дефисами: без дефисов, в скобках, urn — нет
        for bad in [
            "0192f0a17c3b7d2e9a413b5c6d7e8f90",
            "{0192f0a1-7c3b-7d2e-9a41-3b5c6d7e8f90}",
            "urn:uuid:0192f0a1-7c3b-7d2e-9a41-3b5c6d7e8f90",
            "0192f0a1-7c3b-7d2e-9a41-3b5c6d7e8f9z",
            "",
        ] {
            assert!(bad.parse::<SolutionGuid>().is_err(), "{bad}");
        }
        let n = SolutionGuid::new_v7();
        assert_eq!(n.as_str().len(), 36);
        assert_eq!(n.as_str(), n.as_str().to_lowercase());
        assert_ne!(n, SolutionGuid::new_v7());
        let json = serde_json::to_string(&g).unwrap();
        assert_eq!(json, format!("\"{}\"", g.as_str()));
        assert_eq!(serde_json::from_str::<SolutionGuid>(&json).unwrap(), g);
        assert_eq!(
            serde_json::from_str::<SolutionGuid>(&format!("\"{up}\"")).unwrap(),
            g
        );
        assert!(serde_json::from_str::<SolutionGuid>("\"bad\"").is_err());
    }
}
