//! JSON с сохранением порядка ключей для слияния настроек агента (`.claude/settings.json`,
//! `.codex/hooks.json`, `.mcp.json`): чужие ключи остаются на своих местах.
//!
//! `serde_json::Map` в сборке клиента сортирует ключи (без `preserve_order`; включать его
//! нельзя — фича объединилась бы с сервером в общей сборке рабочего пространства и поменяла бы
//! порядок его ответов), поэтому — своё дерево значений поверх парсера `serde_json`.
use std::fmt;

use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::ser::{Serialize, SerializeMap, SerializeSeq, Serializer};
use serde_json::{Number, Value};

/// Значение JSON; у объекта — пары в порядке файла.
#[derive(Debug, Clone, PartialEq)]
pub enum J {
    Null,
    Bool(bool),
    Num(Number),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

impl J {
    /// Разбор текста; ошибка — с номером строки и столбца.
    pub fn parse(text: &str) -> Result<J, String> {
        serde_json::from_str(text).map_err(|e| e.to_string())
    }

    /// Текст с отступом в 2 пробела и переводом строки в конце.
    pub fn pretty(&self) -> String {
        let mut s = serde_json::to_string_pretty(self).unwrap_or_default();
        s.push('\n');
        s
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            J::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn get(&self, key: &str) -> Option<&J> {
        match self {
            J::Obj(p) => p.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut J> {
        match self {
            J::Obj(p) => p.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Значение ключа: замена на месте или добавление в конец объекта.
    pub fn set(&mut self, key: &str, v: J) {
        if let J::Obj(p) = self {
            match p.iter_mut().find(|(k, _)| k == key) {
                Some((_, old)) => *old = v,
                None => p.push((key.to_string(), v)),
            }
        }
    }

    /// Значение ключа-объекта (создаётся пустым в конце, если его нет).
    pub fn entry_obj(&mut self, key: &str) -> Option<&mut J> {
        if self.get(key).is_none() {
            self.set(key, J::Obj(Vec::new()));
        }
        self.get_mut(key)
    }
}

impl From<&Value> for J {
    fn from(v: &Value) -> J {
        match v {
            Value::Null => J::Null,
            Value::Bool(b) => J::Bool(*b),
            Value::Number(n) => J::Num(n.clone()),
            Value::String(s) => J::Str(s.clone()),
            Value::Array(a) => J::Arr(a.iter().map(J::from).collect()),
            Value::Object(o) => J::Obj(o.iter().map(|(k, v)| (k.clone(), J::from(v))).collect()),
        }
    }
}

impl Serialize for J {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            J::Null => s.serialize_unit(),
            J::Bool(b) => s.serialize_bool(*b),
            J::Num(n) => n.serialize(s),
            J::Str(v) => s.serialize_str(v),
            J::Arr(a) => {
                let mut seq = s.serialize_seq(Some(a.len()))?;
                for v in a {
                    seq.serialize_element(v)?;
                }
                seq.end()
            }
            J::Obj(p) => {
                let mut m = s.serialize_map(Some(p.len()))?;
                for (k, v) in p {
                    m.serialize_entry(k, v)?;
                }
                m.end()
            }
        }
    }
}

struct JVisitor;

impl<'de> Visitor<'de> for JVisitor {
    type Value = J;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("значение JSON")
    }

    fn visit_bool<E>(self, v: bool) -> Result<J, E> {
        Ok(J::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<J, E> {
        Ok(J::Num(v.into()))
    }

    fn visit_u64<E>(self, v: u64) -> Result<J, E> {
        Ok(J::Num(v.into()))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<J, E> {
        Number::from_f64(v)
            .map(J::Num)
            .ok_or_else(|| E::custom("число вне JSON"))
    }

    fn visit_str<E>(self, v: &str) -> Result<J, E> {
        Ok(J::Str(v.to_string()))
    }

    fn visit_string<E>(self, v: String) -> Result<J, E> {
        Ok(J::Str(v))
    }

    fn visit_unit<E>(self) -> Result<J, E> {
        Ok(J::Null)
    }

    fn visit_none<E>(self) -> Result<J, E> {
        Ok(J::Null)
    }

    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<J, D::Error> {
        J::deserialize(d)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<J, A::Error> {
        let mut v = Vec::new();
        while let Some(x) = a.next_element()? {
            v.push(x);
        }
        Ok(J::Arr(v))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<J, A::Error> {
        // повтор ключа — последнее значение на месте первого (как у JSON.parse)
        let mut obj = J::Obj(Vec::new());
        while let Some((k, v)) = a.next_entry::<String, J>()? {
            obj.set(&k, v);
        }
        Ok(obj)
    }
}

impl<'de> Deserialize<'de> for J {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<J, D::Error> {
        d.deserialize_any(JVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_key_order_and_values() {
        let text = r#"{"z": 1, "a": {"y": [true, null, 1.5, "s"], "b": -3}, "m": {}}"#;
        let j = J::parse(text).unwrap();
        let out = serde_json::to_string(&j).unwrap();
        assert_eq!(
            out,
            r#"{"z":1,"a":{"y":[true,null,1.5,"s"],"b":-3},"m":{}}"#
        );
        assert_eq!(J::parse(&j.pretty()).unwrap(), j);
    }

    #[test]
    fn set_replaces_in_place_or_appends() {
        let mut j = J::parse(r#"{"a": 1, "b": 2}"#).unwrap();
        j.set("a", J::Str("x".into()));
        j.set("c", J::Null);
        assert_eq!(
            serde_json::to_string(&j).unwrap(),
            r#"{"a":"x","b":2,"c":null}"#
        );
    }

    #[test]
    fn parse_error_has_position() {
        let e = J::parse("{\n  \"a\": ,\n}").unwrap_err();
        assert!(e.contains("line 2"), "{e}");
    }
}
