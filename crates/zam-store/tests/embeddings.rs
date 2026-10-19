//! Кэш эмбеддингов (§6.3 ТЗ): dense f16 LE, sparse (u32 id, f16 вес) LE.

use std::path::PathBuf;

use zam_core::Id;
use zam_store::{DataDir, Db, Embedding, UserDbs, get_embeddings, put_embeddings};

async fn setup(name: &str) -> Db {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("emb-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    let dd = DataDir::open(&dir).unwrap();
    UserDbs::new(dd.users_dir()).get(Id::new()).await.unwrap()
}

fn sample(seed: f32) -> Embedding {
    Embedding {
        dense: (0..1024)
            .map(|i| ((i as f32 * 0.37 + seed).sin()) * 0.5)
            .collect(),
        sparse: vec![(0, 0.125), (17, 0.9), (250_001, 0.0031), (u32::MAX, 1.5)],
    }
}

#[tokio::test]
async fn f16_roundtrip_and_sparse() {
    let db = setup("roundtrip").await;
    let (a, b) = (sample(0.0), sample(1.0));
    put_embeddings(
        &db,
        "bge-m3@rev",
        vec![("h1".into(), a.clone()), ("h2".into(), b.clone())],
    )
    .await
    .unwrap();

    let got = get_embeddings(&db, "bge-m3@rev", &["h1".into(), "h2".into(), "нет".into()])
        .await
        .unwrap();
    assert_eq!(got.len(), 2);
    for (h, want) in [("h1", &a), ("h2", &b)] {
        let e = &got[h];
        assert_eq!(e.dense.len(), want.dense.len());
        let max_err = e
            .dense
            .iter()
            .zip(&want.dense)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0f32, f32::max);
        assert!(max_err <= 1e-3, "{h}: {max_err}");
        assert_eq!(e.sparse.len(), want.sparse.len());
        for ((i, w), (wi, ww)) in e.sparse.iter().zip(&want.sparse) {
            assert_eq!(i, wi);
            assert!(
                (w - ww).abs() <= 1e-3 * ww.abs().max(1.0),
                "{i}: {w} vs {ww}"
            );
        }
    }

    // другая модель — другой ключ; повторная запись заменяет
    assert!(
        get_embeddings(&db, "other", &["h1".into()])
            .await
            .unwrap()
            .is_empty()
    );
    let c = Embedding {
        dense: vec![1.0, -2.0],
        sparse: vec![],
    };
    put_embeddings(&db, "bge-m3@rev", vec![("h1".into(), c.clone())])
        .await
        .unwrap();
    let got = get_embeddings(&db, "bge-m3@rev", &["h1".into()])
        .await
        .unwrap();
    assert_eq!(got["h1"], c);
    assert!(
        get_embeddings(&db, "bge-m3@rev", &[])
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn corrupt_blob_is_error() {
    let db = setup("corrupt").await;
    db.write(|tx| {
        tx.execute(
            "INSERT INTO embeddings (text_hash, model_id, dense, sparse, created_at)
             VALUES ('h', 'm', x'0000aa', x'', '2026-10-19T00:00:00.000000Z')",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let err = get_embeddings(&db, "m", &["h".into()]).await.unwrap_err();
    assert!(matches!(err, zam_store::StoreError::Corrupt(_)), "{err}");
}
