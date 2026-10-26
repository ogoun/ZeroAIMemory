//! Вложения (§6.1, §6.5 ТЗ): файлы блобов пользователя и их привязка к заметкам.
//!
//! **Файлы.** `<data_dir>/blobs/<user_id>/<aa>/<sha256>`, где `aa` — первые два
//! символа хэша; одинаковое содержимое хранится один раз в пределах пользователя.
//! Запись атомарна: поток пишется во временный `<root>/.incoming/<id>.part` с
//! подсчётом sha256 и проверкой лимита на лету, затем `fsync` и `rename` на место.
//! Если файл с таким хэшем уже есть, временный просто удаляется. При любой ошибке
//! (лимит, ввод-вывод, обрыв потока) временный файл удаляется.
//!
//! **Таблицы.** `attachments` — пара (узел, sha256, имя файла) с MIME и размером;
//! `blobs.refcount` — число таких различных пар (повторная привязка той же пары
//! счётчик не меняет). Сборщик мусора (`refcount = 0`) — не в M1.
//!
//! **Ссылка** в Markdown: `_attachments/<sha256[0..16]>/<имя>`; в имени
//! percent-кодируются только символы, ломающие синтаксис ссылки Markdown
//! (пробел, `(`, `)`, `<`, `>`) и символы, которые рендер трактует особо
//! (`%` — начало кода, `?` и `#` — запрос и якорь URL).

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use rusqlite::{OptionalExtension, params};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use zam_core::Id;

use crate::certainty::{CertaintyChange, check_certainty};
use crate::entries::Certainty;
use crate::nodes::AuthorKind;
use crate::paths::NodePath;
use crate::time::fmt_ts;
use crate::{Db, StoreError};

/// Каталог временных файлов загрузки внутри корня пользователя.
const INCOMING: &str = ".incoming";
/// Размер буфера чтения потока, байт.
const BUF: usize = 64 * 1024;
/// Длина префикса хэша в ссылке.
pub const LINK_SHA_LEN: usize = 16;

/// Файлы блобов одного пользователя.
#[derive(Debug, Clone)]
pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    /// `blobs_dir` — [`crate::DataDir::blobs_dir`]; корень — `<blobs_dir>/<user>`.
    pub fn new(blobs_dir: &Path, user: Id) -> BlobStore {
        BlobStore {
            root: blobs_dir.join(user.to_string()),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Путь файла блоба (без проверки существования).
    pub fn path_of(&self, sha256: &str) -> PathBuf {
        let aa = sha256.get(..2).unwrap_or("00");
        self.root.join(aa).join(sha256)
    }

    /// Поток → временный файл → sha256 → файл на месте. Больше `limit` байт →
    /// `BadInput("too large")`. Возвращает `(sha256 hex, размер)`.
    pub async fn put<R: AsyncRead + Unpin>(
        &self,
        r: R,
        limit: u64,
    ) -> Result<(String, u64), StoreError> {
        let incoming = self.root.join(INCOMING);
        tokio::fs::create_dir_all(&incoming).await?;
        let part = incoming.join(format!("{}.part", Id::new()));
        match self.write_part(r, limit, &part).await {
            Ok((sha, size)) => match self.commit(&part, &sha).await {
                Ok(()) => Ok((sha, size)),
                Err(e) => {
                    let _ = tokio::fs::remove_file(&part).await;
                    Err(e)
                }
            },
            Err(e) => {
                let _ = tokio::fs::remove_file(&part).await;
                Err(e)
            }
        }
    }

    async fn write_part<R: AsyncRead + Unpin>(
        &self,
        mut r: R,
        limit: u64,
        part: &Path,
    ) -> Result<(String, u64), StoreError> {
        let mut f = tokio::fs::File::create(part).await?;
        let mut hasher = Sha256::new();
        let mut size: u64 = 0;
        let mut buf = vec![0u8; BUF];
        loop {
            let n = r.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            size += n as u64;
            if size > limit {
                return Err(StoreError::BadInput("too large".into()));
            }
            hasher.update(&buf[..n]);
            f.write_all(&buf[..n]).await?;
        }
        f.flush().await?;
        f.sync_all().await?;
        drop(f);
        Ok((hex(&hasher.finalize()), size))
    }

    /// Временный файл → `<root>/<aa>/<sha256>`; уже есть — временный удаляется.
    async fn commit(&self, part: &Path, sha: &str) -> Result<(), StoreError> {
        let target = self.path_of(sha);
        if let Some(dir) = target.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        if tokio::fs::try_exists(&target).await? {
            tokio::fs::remove_file(part).await?;
            return Ok(());
        }
        match tokio::fs::rename(part, &target).await {
            Ok(()) => Ok(()),
            // параллельная загрузка того же содержимого успела раньше (Windows не
            // перезаписывает существующий файл при rename)
            Err(_) if tokio::fs::try_exists(&target).await.unwrap_or(false) => {
                tokio::fs::remove_file(part).await?;
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// 64 символа hex в нижнем регистре.
pub fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Имя файла вложения — по правилам одного сегмента [`NodePath`].
pub fn check_filename(name: &str) -> Result<(), StoreError> {
    if name.contains('/') {
        return Err(StoreError::BadInput(
            "имя файла не может содержать «/»".into(),
        ));
    }
    NodePath::parse(name).map(|_| ())
}

/// `_attachments/<sha[0..16]>/<имя>` (см. модуль о кодировании имени).
pub fn attachment_link(sha256: &str, filename: &str) -> String {
    let mut name = String::with_capacity(filename.len());
    for ch in filename.chars() {
        match ch {
            ' ' | '(' | ')' | '<' | '>' | '%' | '?' | '#' => {
                name.push_str(&format!("%{:02X}", ch as u32));
            }
            _ => name.push(ch),
        }
    }
    format!(
        "_attachments/{}/{name}",
        &sha256[..LINK_SHA_LEN.min(sha256.len())]
    )
}

/// Привязывает блоб к живой заметке `node`; возвращает ссылку для Markdown.
/// Новая пара (узел, sha256, имя) добавляет строку `attachments` и увеличивает
/// `blobs.refcount` (или создаёт блоб); повтор пары ничего не меняет. Вложение — изменение
/// заметки: к заметке `axiom` его добавляет только человек (`author`), иначе `Certainty`.
pub async fn attach(
    db: &Db,
    node: Id,
    sha256: &str,
    size: u64,
    filename: &str,
    mime: &str,
    author: AuthorKind,
) -> Result<String, StoreError> {
    if !is_sha256(sha256) {
        return Err(StoreError::BadInput("sha256: 64 символа hex".into()));
    }
    check_filename(filename)?;
    let link = attachment_link(sha256, filename);
    let size = i64::try_from(size).map_err(|_| StoreError::BadInput("размер".into()))?;
    let (sha, name, mime) = (sha256.to_string(), filename.to_string(), mime.to_string());
    db.write(move |t| {
        let live_note: Option<Option<Certainty>> = t
            .query_row(
                "SELECT certainty FROM nodes
                 WHERE id = ?1 AND kind = 'note' AND deleted_at IS NULL",
                [node],
                |r| r.get(0),
            )
            .optional()?;
        let Some(certainty) = live_note else {
            return Err(StoreError::NotFound);
        };
        check_certainty(author, certainty, &CertaintyChange::ModifyOther, None, None)?;
        let now = fmt_ts(Timestamp::now());
        t.execute(
            "INSERT INTO blobs (sha256, size, mime, refcount, created_at)
             VALUES (?1, ?2, ?3, 0, ?4) ON CONFLICT (sha256) DO NOTHING",
            params![sha, size, mime, now],
        )?;
        let new_pair = t.execute(
            "INSERT INTO attachments (node_id, blob_sha256, filename, mime, size, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (node_id, blob_sha256, filename) DO NOTHING",
            params![node, sha, name, mime, size, now],
        )?;
        if new_pair == 1 {
            t.execute(
                "UPDATE blobs SET refcount = refcount + 1 WHERE sha256 = ?1",
                [&sha],
            )?;
        }
        Ok(())
    })
    .await?;
    Ok(link)
}

/// Где искать вложение при разрешении ссылки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttachScope {
    /// Пространство рендеримой заметки: его вложения предпочитаются.
    pub prefer_space: Option<Id>,
    /// `false` — вложения узлов пространства Human не видны (запрос по токену).
    pub allow_human: bool,
    /// `true` — только пространство `prefer_space` (участник общего решения в базе
    /// владельца не видит вложений других его пространств, §11.6).
    pub only_space: bool,
}

/// Ссылка `_attachments/<sha16>/<имя>` → `(sha256, mime)`. `sha16` — без учёта
/// регистра. Порядок выбора: пространство `prefer_space`, затем живой узел,
/// затем более новая привязка.
pub async fn resolve_attachment(
    db: &Db,
    sha16: &str,
    filename: &str,
    scope: AttachScope,
) -> Result<Option<(String, String)>, StoreError> {
    let sha16 = sha16.to_ascii_lowercase();
    if sha16.len() != LINK_SHA_LEN || !sha16.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(None);
    }
    let name = filename.to_string();
    db.read(move |c| {
        Ok(c.query_row(
            "SELECT a.blob_sha256, COALESCE(a.mime, 'application/octet-stream')
             FROM attachments a
             JOIN nodes n ON n.id = a.node_id
             JOIN spaces s ON s.id = n.space_id
             WHERE substr(a.blob_sha256, 1, 16) = ?1 AND a.filename = ?2
               AND (?3 OR s.kind <> 'human')
               AND (NOT ?5 OR n.space_id IS ?4)
             ORDER BY (n.space_id IS ?4) DESC, (n.deleted_at IS NULL) DESC, a.created_at DESC
             LIMIT 1",
            params![
                sha16,
                name,
                scope.allow_human,
                scope.prefer_space,
                scope.only_space
            ],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
    })
    .await
}

/// MIME и размер пары (sha256, имя), если она привязана хоть к одному узлу
/// доступного пространства (`allow_human = false` — без Human).
pub async fn blob_meta(
    db: &Db,
    sha256: &str,
    filename: &str,
    allow_human: bool,
) -> Result<Option<(String, u64)>, StoreError> {
    let (sha, name) = (sha256.to_string(), filename.to_string());
    db.read(move |c| {
        let row: Option<(String, i64)> = c
            .query_row(
                "SELECT COALESCE(a.mime, 'application/octet-stream'), a.size
                 FROM attachments a
                 JOIN nodes n ON n.id = a.node_id
                 JOIN spaces s ON s.id = n.space_id
                 WHERE a.blob_sha256 = ?1 AND a.filename = ?2
                   AND (?3 OR s.kind <> 'human')
                 ORDER BY (n.deleted_at IS NULL) DESC, a.created_at DESC
                 LIMIT 1",
                params![sha, name, allow_human],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(row.map(|(m, s)| (m, u64::try_from(s).unwrap_or(0))))
    })
    .await
}

/// MIME и размер пары (sha256, имя), если она привязана хоть к одному узлу пространства
/// `space` (участник общего решения в базе владельца — только пространство решения).
pub async fn blob_meta_in_space(
    db: &Db,
    sha256: &str,
    filename: &str,
    space: Id,
) -> Result<Option<(String, u64)>, StoreError> {
    let (sha, name) = (sha256.to_string(), filename.to_string());
    db.read(move |c| {
        let row: Option<(String, i64)> = c
            .query_row(
                "SELECT COALESCE(a.mime, 'application/octet-stream'), a.size
                 FROM attachments a
                 JOIN nodes n ON n.id = a.node_id
                 WHERE a.blob_sha256 = ?1 AND a.filename = ?2 AND n.space_id = ?3
                 ORDER BY (n.deleted_at IS NULL) DESC, a.created_at DESC
                 LIMIT 1",
                params![sha, name, space],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(row.map(|(m, s)| (m, u64::try_from(s).unwrap_or(0))))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_encoding() {
        let sha = "ab".repeat(32);
        assert_eq!(
            attachment_link(&sha, "a b(c)<d>%?#.png"),
            format!(
                "_attachments/{}/a%20b%28c%29%3Cd%3E%25%3F%23.png",
                &sha[..16]
            )
        );
        assert_eq!(
            attachment_link(&sha, "кот.png"),
            format!("_attachments/{}/кот.png", &sha[..16])
        );
    }

    #[test]
    fn sha_check() {
        assert!(is_sha256(&"0f".repeat(32)));
        assert!(!is_sha256(&"0F".repeat(32)));
        assert!(!is_sha256(&"0f".repeat(31)));
    }
}
