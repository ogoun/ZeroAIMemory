//! Порядок счётчика выдачи (M6, задача 5, решение владельца 2026-11-06): токенизатор модели →
//! `tokenizer.json` комплекта → оценка с запасом. Чанкер без модели режет оценкой и при
//! счётчике комплекта. Отдельный процесс теста: счётчики процесса глобальные.

use std::sync::Arc;

use zam_markdown::{EstimateCounter, TokenCounter};
use zam_store::{
    BudgetCounterKind, budget_cap, budget_counter_kind, budget_tokenizer_id, count_tokens,
    fits_budget, set_bundled_counter, set_token_counter, token_counter_exact,
};

/// Точный счётчик для проверки: токен — слово.
struct Words(&'static str);

impl TokenCounter for Words {
    fn count(&self, text: &str) -> usize {
        text.split_whitespace().count()
    }
    fn is_exact(&self) -> bool {
        true
    }
    fn tokenizer_id(&self) -> String {
        self.0.into()
    }
}

#[test]
fn budget_counter_order_model_bundled_estimate() {
    let t = "alpha beta gamma delta epsilon";
    // ни модели, ни комплекта — оценка с запасом
    assert_eq!(budget_counter_kind(), BudgetCounterKind::Estimate);
    assert_eq!(budget_tokenizer_id(), EstimateCounter.tokenizer_id());
    assert_eq!(count_tokens(t), EstimateCounter.count(t));
    assert!(budget_cap(800) < 800);
    // tokenizer.json комплекта — точный счёт выдачи, чанкер по-прежнему оценкой
    set_bundled_counter(Arc::new(Words("bundled")));
    assert_eq!(budget_counter_kind(), BudgetCounterKind::Bundled);
    assert_eq!(budget_counter_kind().as_str(), "bundled");
    assert_eq!(budget_tokenizer_id(), "bundled");
    assert_eq!(count_tokens(t), 5);
    assert_eq!(budget_cap(800), 800);
    assert!(fits_budget(t, 5) && !fits_budget(t, 4));
    assert!(!token_counter_exact(), "чанкер без модели — оценка");
    // модель установлена — её токенизатор (тот же, что у чанкера)
    set_token_counter(Arc::new(Words("model")));
    assert_eq!(budget_counter_kind(), BudgetCounterKind::Model);
    assert!(token_counter_exact());
    assert_eq!(budget_tokenizer_id(), "model");
}
