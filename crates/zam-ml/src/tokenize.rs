//! Токенизатор Hugging Face (`tokenizer.json`) и счётчик токенов.
//!
//! Один экземпляр токенизатора на модель, без усечения и паддинга из `tokenizer.json` (у MiniLM
//! там зашиты усечение и паддинг до 128): счётчик считает текст целиком и без спецтокенов, а для
//! инференса текст усекается и обрамляется спецтокенами здесь же ([`Tok::encode`]).

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use sha2::{Digest, Sha256};
use tokenizers::{Encoding, PostProcessor as _, Tokenizer, TruncationDirection};
use zam_markdown::{ApproxCounter, TokenCounter};

use crate::MlError;
use crate::models::{TOKENIZER_FILE, model_dir, require_file};

/// Предел байт текста на один токен усечения: длиннее текст до токенизации не доходит
/// (токен bge-m3 в среднем 3–5 байт UTF-8, запас на редкие длинные токены).
const MAX_BYTES_PER_TOKEN: usize = 16;

/// Префикс `text` не длиннее `max_tokens * 16` байт по границе символа: ограничивает работу
/// токенизатора и размер ключа LRU для заведомо усекаемых текстов.
pub(crate) fn cap_text(text: &str, max_tokens: usize) -> &str {
    let limit = max_tokens.saturating_mul(MAX_BYTES_PER_TOKEN);
    if text.len() <= limit {
        text
    } else {
        &text[..text.floor_char_boundary(limit)]
    }
}

/// Токенизатор, его идентификатор и признак того, что счётчик хотя бы раз откатился на оценку.
pub(crate) struct Shared {
    tok: Tokenizer,
    /// `sha256:<первые 16 hex>` содержимого `tokenizer.json` ([`TokenCounter::tokenizer_id`]):
    /// у моделей с одним токенизатором (bge-m3 int8 и fp16) совпадает.
    id: String,
    degraded: AtomicBool,
}

impl Shared {
    fn new(tok: Tokenizer, id: String) -> Arc<Shared> {
        Arc::new(Shared {
            tok,
            id,
            degraded: AtomicBool::new(false),
        })
    }

    /// Число токенов без спецтокенов; при ошибке токенизатора — оценка с предупреждением.
    fn count(&self, text: &str) -> usize {
        match self.tok.encode_fast(text, false) {
            Ok(e) => e.len(),
            Err(e) => self.fallback(text, &e.to_string()),
        }
    }

    /// Оценка вместо точного подсчёта: оценка лучше паники в транзакции записи.
    /// Текст в лог не пишется; после первого отката счётчик сообщает `is_exact() == false`.
    fn fallback(&self, text: &str, err: &str) -> usize {
        if !self.degraded.swap(true, Ordering::Relaxed) {
            tracing::warn!(error = %err, bytes = text.len(), "токенизатор не справился, счётчик перешёл на оценку");
        } else {
            tracing::debug!(error = %err, bytes = text.len(), "оценка числа токенов");
        }
        ApproxCounter.count(text)
    }

    fn is_exact(&self) -> bool {
        !self.degraded.load(Ordering::Relaxed)
    }
}

/// Токенизатор модели с параметрами обрамления.
pub(crate) struct Tok {
    pub inner: Arc<Shared>,
    /// Сколько спецтокенов добавляет пост-процессор к одному тексту.
    n_added: usize,
    /// Сколько спецтокенов добавляет пост-процессор к паре (реранкер: `<s> q </s></s> p </s>`).
    n_added_pair: usize,
    /// Токен паддинга.
    pub pad_id: u32,
}

fn tok_err(e: impl std::fmt::Display) -> MlError {
    MlError::Tokenizer(e.to_string())
}

/// Идентификатор токенизатора по содержимому `tokenizer.json`.
fn tokenizer_id(bytes: &[u8]) -> String {
    let h = Sha256::digest(bytes);
    let hex: String = h[..8].iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

/// Загружает `tokenizer.json` и снимает зашитые в него усечение и паддинг.
/// Возвращает токенизатор, id паддинга (из `tokenizer.json`, иначе `<pad>`/`[PAD]`, иначе 0)
/// и идентификатор токенизатора ([`tokenizer_id`]).
fn load_raw(path: &Path) -> Result<(Tokenizer, u32, String), MlError> {
    let bad = |e: &dyn std::fmt::Display| MlError::Tokenizer(format!("{}: {e}", path.display()));
    let bytes = std::fs::read(path).map_err(|e| bad(&e))?;
    let id = tokenizer_id(&bytes);
    let mut t = Tokenizer::from_bytes(&bytes).map_err(|e| bad(&e))?;
    let pad_id = t
        .get_padding()
        .map(|p| p.pad_id)
        .or_else(|| t.token_to_id("<pad>"))
        .or_else(|| t.token_to_id("[PAD]"))
        .unwrap_or(0);
    t.with_truncation(None).map_err(tok_err)?;
    t.with_padding(None);
    Ok((t, pad_id, id))
}

impl Tok {
    pub fn load(path: &Path) -> Result<Tok, MlError> {
        let (t, pad_id, id) = load_raw(path)?;
        let n_added = t.get_post_processor().map_or(0, |p| p.added_tokens(false));
        let n_added_pair = t.get_post_processor().map_or(0, |p| p.added_tokens(true));
        Ok(Tok {
            inner: Shared::new(t, id),
            n_added,
            n_added_pair,
            pad_id,
        })
    }

    /// Кодирует пары «запрос — фрагмент» для реранкера: запрос усекается до `max_query`
    /// токенов, фрагмент — до `max_passage`, и пара целиком (со спецтокенами) — до
    /// `max_total` за счёт фрагмента. Спецтокены пары расставляет пост-процессор.
    pub fn encode_pairs(
        &self,
        query: &str,
        passages: &[&str],
        max_query: usize,
        max_passage: usize,
        max_total: usize,
    ) -> Result<Vec<Encoding>, MlError> {
        let tok = &self.inner.tok;
        let mut q = tok
            .encode_fast(cap_text(query, max_query), false)
            .map_err(tok_err)?;
        let q_budget = max_query
            .min(max_total.saturating_sub(self.n_added_pair + 1))
            .max(1);
        if q.len() > q_budget {
            q.truncate(q_budget, 0, TruncationDirection::Right);
            q.take_overflowing();
        }
        let p_budget = max_passage
            .min(max_total.saturating_sub(self.n_added_pair + q.len()))
            .max(1);
        let capped: Vec<&str> = passages.iter().map(|t| cap_text(t, p_budget)).collect();
        let raw = tok.encode_batch_fast(capped, false).map_err(tok_err)?;
        raw.into_iter()
            .map(|mut p| {
                if p.len() > p_budget {
                    p.truncate(p_budget, 0, TruncationDirection::Right);
                    p.take_overflowing();
                }
                tok.post_process(q.clone(), Some(p), true).map_err(tok_err)
            })
            .collect()
    }

    /// Кодирует тексты для модели: усечение до `max_tokens` вместе со спецтокенами
    /// (текст заранее обрезается до `max_tokens * 16` байт).
    pub fn encode(&self, texts: &[&str], max_tokens: usize) -> Result<Vec<Encoding>, MlError> {
        let budget = max_tokens.saturating_sub(self.n_added).max(1);
        let capped: Vec<&str> = texts.iter().map(|t| cap_text(t, max_tokens)).collect();
        let raw = self
            .inner
            .tok
            .encode_batch_fast(capped, false)
            .map_err(tok_err)?;
        raw.into_iter()
            .map(|mut e| {
                if e.len() > budget {
                    e.truncate(budget, 0, TruncationDirection::Right);
                    e.take_overflowing();
                }
                self.inner.tok.post_process(e, None, true).map_err(tok_err)
            })
            .collect()
    }

    /// Число токенов без спецтокенов.
    pub fn count(&self, text: &str) -> usize {
        self.inner.count(text)
    }
}

/// Точный счётчик токенов модели (без спецтокенов: чанкер складывает счётчики частей).
/// Если токенизатор отказал, счётчик отдаёт оценку `ceil(символы / 3,5)`, пишет предупреждение
/// и с этого момента сообщает `is_exact() == false`.
#[derive(Clone)]
pub struct BgeTokenCounter(Arc<Shared>);

impl BgeTokenCounter {
    pub(crate) fn new(t: Arc<Shared>) -> Self {
        BgeTokenCounter(t)
    }

    /// Счётчик по `<models_dir>/<id>/tokenizer.json` без загрузки ORT и модели.
    pub fn load(models_dir: &Path, id: &str) -> Result<BgeTokenCounter, MlError> {
        let path = model_dir(models_dir, id)?.join(TOKENIZER_FILE);
        require_file(&path)?;
        let (t, _, id) = load_raw(&path)?;
        Ok(BgeTokenCounter(Shared::new(t, id)))
    }
}

impl std::fmt::Debug for BgeTokenCounter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BgeTokenCounter")
    }
}

impl TokenCounter for BgeTokenCounter {
    fn count(&self, text: &str) -> usize {
        self.0.count(text)
    }
    fn is_exact(&self) -> bool {
        self.0.is_exact()
    }
    fn tokenizer_id(&self) -> String {
        self.0.id.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_text_on_char_boundary() {
        assert_eq!(cap_text("abc", 1), "abc");
        let s = "я".repeat(20); // 40 байт
        let c = cap_text(&s, 1); // предел 16 байт
        assert_eq!(c, "я".repeat(8));
        let odd = format!("a{s}"); // граница 16 попадает внутрь символа
        assert_eq!(cap_text(&odd, 1), format!("a{}", "я".repeat(7)));
        assert_eq!(cap_text("abc", 0), "");
    }

    #[test]
    fn fallback_marks_counter_inexact() {
        let mut t = Tokenizer::new(tokenizers::models::wordlevel::WordLevel::default());
        t.with_truncation(None).unwrap();
        let sh = Shared::new(t, "test".into());
        let c = BgeTokenCounter::new(sh.clone());
        assert!(c.is_exact());
        assert_eq!(sh.fallback("abcdefg", "сбой"), 2);
        assert!(!c.is_exact());
        assert_eq!(
            c.tokenizer_id(),
            "test",
            "откат на оценку идентификатор не меняет"
        );
    }

    #[test]
    fn tokenizer_id_by_content() {
        assert_eq!(tokenizer_id(b"{}"), tokenizer_id(b"{}"));
        assert_ne!(tokenizer_id(b"{}"), tokenizer_id(b"{ }"));
        assert!(tokenizer_id(b"{}").starts_with("sha256:"));
        assert_eq!(tokenizer_id(b"{}").len(), "sha256:".len() + 16);
    }
}
