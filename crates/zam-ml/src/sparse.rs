//! Sparse-голова bge-m3: `relu(h·w + b)` по каждому токену, max по совпадающим id
//! (как в FlagEmbedding). Веса — `sparse_linear.pt` из `BAAI/bge-m3`: torch-zip без сжатия,
//! `*/data/0` — fp16 `weight[1, dim]`, `*/data/1` — fp16 `bias[1]`. Pickle не исполняется и не
//! разбирается: файл закреплён sha256 в `deploy/models.toml`, достаточно сырых LE-fp16.

use std::collections::HashMap;
use std::io::Read as _;
use std::path::Path;

use crate::MlError;

/// Порог отбрасывания малых весов.
pub(crate) const MIN_WEIGHT: f32 = 1e-4;
/// Спецтокены XLM-R (`<s>`, `<pad>`, `</s>`, `<unk>`) в sparse не входят.
const MAX_SPECIAL_ID: u32 = 3;

/// Линейная sparse-голова `Linear(dim → 1)`.
#[derive(Debug, Clone)]
pub struct SparseHead {
    w: Vec<f32>,
    b: f32,
}

/// Читает единственную запись `*<suffix>` ровно в `expected` байт. Размер из каталога zip
/// не используется для выделения памяти: повреждённый файл даёт `Spec`, а не панику или OOM.
fn read_entry(
    z: &mut zip::ZipArchive<std::fs::File>,
    suffix: &str,
    expected: usize,
) -> Result<Vec<u8>, MlError> {
    let names: Vec<String> = z
        .file_names()
        .filter_map(Result::ok)
        .filter(|n| n.ends_with(suffix))
        .map(|n| n.into_owned())
        .collect();
    let [name] = names.as_slice() else {
        return Err(MlError::Spec(format!(
            "в sparse-голове нужна ровно одна запись *{suffix}, найдено {}",
            names.len()
        )));
    };
    let f = z
        .by_name(name)
        .map_err(|e| MlError::Spec(format!("{name}: {e}")))?;
    if f.size() != expected as u64 {
        return Err(MlError::Spec(format!(
            "{name}: {} Б, ожидалось {expected} (fp16)",
            f.size()
        )));
    }
    let mut buf = Vec::with_capacity(expected);
    f.take(expected as u64 + 1)
        .read_to_end(&mut buf)
        .map_err(|e| MlError::Spec(format!("{name}: {e}")))?;
    if buf.len() != expected {
        return Err(MlError::Spec(format!(
            "{name}: прочитано {} Б, ожидалось {expected}",
            buf.len()
        )));
    }
    Ok(buf)
}

fn f16s(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| half::f16::from_le_bytes(*c).to_f32())
        .collect()
}

impl SparseHead {
    /// Читает `sparse_linear.pt`; размер весов должен совпасть с `dim` модели.
    pub fn load(path: &Path, dim: usize) -> Result<SparseHead, MlError> {
        let f = std::fs::File::open(path)
            .map_err(|e| MlError::NoModel(format!("нет sparse-головы {}: {e}", path.display())))?;
        let mut z = zip::ZipArchive::new(f)
            .map_err(|e| MlError::Spec(format!("{}: не zip ({e})", path.display())))?;
        let ctx = |e: MlError| match e {
            MlError::Spec(m) => MlError::Spec(format!("{} (dim = {dim}): {m}", path.display())),
            e => e,
        };
        let w = read_entry(&mut z, "/data/0", dim * 2).map_err(ctx)?;
        let b = read_entry(&mut z, "/data/1", 2).map_err(ctx)?;
        let w = f16s(&w);
        let b = f16s(&b)[0];
        if !w.iter().all(|x| x.is_finite()) || !b.is_finite() {
            return Err(MlError::Spec(format!(
                "{}: не конечные веса",
                path.display()
            )));
        }
        Ok(SparseHead { w, b })
    }

    /// Размерность входа.
    pub fn dim(&self) -> usize {
        self.w.len()
    }

    /// Sparse-вектор одного текста. `hidden` — `[T, dim]` построчно, `ids` и `mask` — длины `T`.
    /// Токены под нулевой маской и спецтокены (id 0..=3) пропускаются; веса < 1e-4 отбрасываются.
    /// Результат отсортирован по id.
    pub fn pool(&self, hidden: &[f32], ids: &[u32], mask: &[u32]) -> Vec<(u32, f32)> {
        let h = self.w.len();
        // несогласованные длины не паникуют: берётся общая часть
        let n = ids.len().min(mask.len()).min(hidden.len() / h.max(1));
        let (ids, mask) = (&ids[..n], &mask[..n]);
        let mut best: HashMap<u32, f32> = HashMap::new();
        for (t, (&id, &m)) in ids.iter().zip(mask).enumerate() {
            if m == 0 || id <= MAX_SPECIAL_ID {
                continue;
            }
            let row = &hidden[t * h..(t + 1) * h];
            let s = row.iter().zip(&self.w).map(|(x, w)| x * w).sum::<f32>() + self.b;
            if s >= MIN_WEIGHT {
                best.entry(id).and_modify(|v| *v = v.max(s)).or_insert(s);
            }
        }
        let mut out: Vec<(u32, f32)> = best.into_iter().collect();
        out.sort_unstable_by_key(|&(id, _)| id);
        out
    }
}
