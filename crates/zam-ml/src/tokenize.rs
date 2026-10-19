//! Токенизатор Hugging Face (`tokenizer.json`) и счётчик токенов.
//!
//! Один экземпляр токенизатора на модель, без усечения и паддинга из `tokenizer.json` (у MiniLM
//! там зашиты усечение и паддинг до 128): счётчик считает текст целиком и без спецтокенов, а для
//! инференса текст усекается и обрамляется спецтокенами здесь же ([`Tok::encode`]).

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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

/// Токенизатор и признак того, что счётчик хотя бы раз откатился на оценку.
pub(crate) struct Shared {
    tok: Tokenizer,
    degraded: AtomicBool,
}

impl Shared {
    fn new(tok: Tokenizer) -> Arc<Shared> {
        Arc::new(Shared {
            tok,
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
    /// Токен паддинга.
    pub pad_id: u32,
}

fn tok_err(e: impl std::fmt::Display) -> MlError {
    MlError::Tokenizer(e.to_string())
}

/// Загружает `tokenizer.json` и снимает зашитые в него усечение и паддинг.
/// Возвращает токенизатор и id паддинга (из `tokenizer.json`, иначе `<pad>`/`[PAD]`, иначе 0).
fn load_raw(path: &Path) -> Result<(Tokenizer, u32), MlError> {
    let mut t = Tokenizer::from_file(path)
        .map_err(|e| MlError::Tokenizer(format!("{}: {e}", path.display())))?;
    let pad_id = t
        .get_padding()
        .map(|p| p.pad_id)
        .or_else(|| t.token_to_id("<pad>"))
        .or_else(|| t.token_to_id("[PAD]"))
        .unwrap_or(0);
    t.with_truncation(None).map_err(tok_err)?;
    t.with_padding(None);
    Ok((t, pad_id))
}

impl Tok {
    pub fn load(path: &Path) -> Result<Tok, MlError> {
        let (t, pad_id) = load_raw(path)?;
        let n_added = t.get_post_processor().map_or(0, |p| p.added_tokens(false));
        Ok(Tok {
            inner: Shared::new(t),
            n_added,
            pad_id,
        })
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
        Ok(BgeTokenCounter(Shared::new(load_raw(&path)?.0)))
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
        let sh = Shared::new(t);
        let c = BgeTokenCounter::new(sh.clone());
        assert!(c.is_exact());
        assert_eq!(sh.fallback("abcdefg", "сбой"), 2);
        assert!(!c.is_exact());
    }
}
