//! Путь узла внутри пространства: `"a/b/c"`.
//!
//! Путь хранится как прислан: Unicode не нормализуется, регистр не меняется.

use std::fmt;

use crate::StoreError;

/// Предел длины одного сегмента, байт.
pub const SEGMENT_MAX_BYTES: usize = 255;
/// Предел длины всего пути, байт.
pub const PATH_MAX_BYTES: usize = 1024;

/// Проверенный путь узла: сегменты через `/`, без ведущего и замыкающего `/`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodePath(String);

impl NodePath {
    /// Проверяет путь. Ошибка — [`StoreError::BadInput`] с названием нарушенного правила.
    pub fn parse(s: &str) -> Result<NodePath, StoreError> {
        let bad = |m: String| Err(StoreError::BadInput(m));
        if s.is_empty() {
            return bad("путь узла пуст".into());
        }
        if s.len() > PATH_MAX_BYTES {
            return bad(format!("путь длиннее {PATH_MAX_BYTES} байт"));
        }
        if s.starts_with('/') || s.ends_with('/') {
            return bad("путь не может начинаться или заканчиваться на «/»".into());
        }
        if s.contains('\\') {
            return bad("путь не может содержать «\\»".into());
        }
        if s.chars().any(char::is_control) {
            return bad("путь не может содержать управляющие символы".into());
        }
        for seg in s.split('/') {
            if seg.is_empty() {
                return bad("пустой сегмент пути («//»)".into());
            }
            if seg == "." || seg == ".." {
                return bad("сегменты «.» и «..» запрещены".into());
            }
            if seg.len() > SEGMENT_MAX_BYTES {
                return bad(format!("сегмент пути длиннее {SEGMENT_MAX_BYTES} байт"));
            }
        }
        Ok(NodePath(s.to_string()))
    }

    /// Путь родителя; `None` для узла в корне пространства.
    pub fn parent(&self) -> Option<NodePath> {
        self.0
            .rsplit_once('/')
            .map(|(p, _)| NodePath(p.to_string()))
    }

    /// Последний сегмент — имя узла.
    pub fn name(&self) -> &str {
        self.0.rsplit_once('/').map_or(&self.0, |(_, n)| n)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Пути предков от корня, без самого узла: `a/b/c` → `a`, `a/b`.
    pub fn ancestors(&self) -> impl Iterator<Item = &str> {
        self.0.match_indices('/').map(move |(i, _)| &self.0[..i])
    }
}

impl fmt::Display for NodePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ancestors_in_order() {
        let p = NodePath::parse("a/бб/c").unwrap();
        assert_eq!(p.ancestors().collect::<Vec<_>>(), vec!["a", "a/бб"]);
        assert_eq!(NodePath::parse("a").unwrap().ancestors().count(), 0);
    }
}
