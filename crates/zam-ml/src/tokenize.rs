//! Токенизатор Hugging Face (`tokenizer.json`) и счётчик токенов.
//!
//! Один экземпляр токенизатора на модель, без усечения и паддинга из `tokenizer.json` (у MiniLM
//! там зашиты усечение и паддинг до 128): счётчик считает текст целиком и без спецтокенов, а для
//! инференса текст усекается и обрамляется спецтокенами здесь же ([`Tok::encode`]).

use std::path::Path;
use std::sync::Arc;

use tokenizers::{Encoding, PostProcessor as _, Tokenizer, TruncationDirection};
use zam_markdown::{ApproxCounter, TokenCounter};

use crate::MlError;
use crate::models::{TOKENIZER_FILE, model_dir, require_file};

/// Токенизатор модели с параметрами обрамления.
pub(crate) struct Tok {
    pub inner: Arc<Tokenizer>,
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
            inner: Arc::new(t),
            n_added,
            pad_id,
        })
    }

    /// Кодирует тексты для модели: усечение до `max_tokens` вместе со спецтокенами.
    pub fn encode(&self, texts: &[&str], max_tokens: usize) -> Result<Vec<Encoding>, MlError> {
        let budget = max_tokens.saturating_sub(self.n_added).max(1);
        let raw = self
            .inner
            .encode_batch_fast(texts.to_vec(), false)
            .map_err(tok_err)?;
        raw.into_iter()
            .map(|mut e| {
                if e.len() > budget {
                    e.truncate(budget, 0, TruncationDirection::Right);
                    e.take_overflowing();
                }
                self.inner.post_process(e, None, true).map_err(tok_err)
            })
            .collect()
    }

    /// Число токенов без спецтокенов.
    pub fn count(&self, text: &str) -> usize {
        count(&self.inner, text)
    }
}

fn count(t: &Tokenizer, text: &str) -> usize {
    match t.encode_fast(text, false) {
        Ok(e) => e.len(),
        // не должно случаться; оценка лучше паники в транзакции записи
        Err(_) => ApproxCounter.count(text),
    }
}

/// Точный счётчик токенов модели (без спецтокенов: чанкер складывает счётчики частей).
#[derive(Clone)]
pub struct BgeTokenCounter(Arc<Tokenizer>);

impl BgeTokenCounter {
    pub(crate) fn new(t: Arc<Tokenizer>) -> Self {
        BgeTokenCounter(t)
    }

    /// Счётчик по `<models_dir>/<id>/tokenizer.json` без загрузки ORT и модели.
    pub fn load(models_dir: &Path, id: &str) -> Result<BgeTokenCounter, MlError> {
        let path = model_dir(models_dir, id)?.join(TOKENIZER_FILE);
        require_file(&path)?;
        Ok(BgeTokenCounter(Arc::new(load_raw(&path)?.0)))
    }
}

impl std::fmt::Debug for BgeTokenCounter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BgeTokenCounter")
    }
}

impl TokenCounter for BgeTokenCounter {
    fn count(&self, text: &str) -> usize {
        count(&self.0, text)
    }
    fn is_exact(&self) -> bool {
        true
    }
}
