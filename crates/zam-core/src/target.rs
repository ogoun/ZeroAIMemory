//! Ссылка на цель (узел или запись журнала): `n:<id>` | `e:<id>`.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{Id, ParseIdError};

/// Вид цели.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TargetKind {
    Node,
    Entry,
}

impl TargetKind {
    /// Префикс текстовой формы: `n` | `e`.
    fn prefix(self) -> &'static str {
        match self {
            TargetKind::Node => "n",
            TargetKind::Entry => "e",
        }
    }
}

/// Узел или запись журнала. Текстовая форма (`Display`/`FromStr`, serde) — `n:<id>` | `e:<id>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TargetRef {
    pub kind: TargetKind,
    pub id: Id,
}

impl fmt::Display for TargetRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.kind.prefix(), self.id)
    }
}

impl FromStr for TargetRef {
    type Err = ParseIdError;

    fn from_str(s: &str) -> Result<TargetRef, ParseIdError> {
        let (k, id) = s.split_once(':').ok_or(ParseIdError)?;
        let kind = match k {
            "n" => TargetKind::Node,
            "e" => TargetKind::Entry,
            _ => return Err(ParseIdError),
        };
        Ok(TargetRef {
            kind,
            id: id.parse()?,
        })
    }
}

impl Serialize for TargetRef {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for TargetRef {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<TargetRef, D::Error> {
        let s = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_ref_display_parse() {
        let id = Id::new();
        let n = TargetRef {
            kind: TargetKind::Node,
            id,
        };
        let e = TargetRef {
            kind: TargetKind::Entry,
            id,
        };
        assert_eq!(n.to_string(), format!("n:{id}"));
        assert_eq!(e.to_string(), format!("e:{id}"));
        assert_eq!(n.to_string().parse::<TargetRef>().unwrap(), n);
        assert_eq!(e.to_string().parse::<TargetRef>().unwrap(), e);
        // id принимается в любой форме, которую понимает Id
        assert_eq!(
            format!("e:{}", id.as_uuid()).parse::<TargetRef>().unwrap(),
            e
        );
        for bad in [
            "",
            "n:",
            "x:1",
            &id.to_string(),
            &format!("N:{id}"),
            "n:мусор",
        ] {
            assert!(bad.parse::<TargetRef>().is_err(), "{bad}");
        }
        assert_eq!(
            serde_json::to_string(&TargetKind::Entry).unwrap(),
            "\"entry\""
        );
        let json = serde_json::to_string(&n).unwrap();
        assert_eq!(json, format!("\"n:{id}\""));
        assert_eq!(serde_json::from_str::<TargetRef>(&json).unwrap(), n);
    }
}
