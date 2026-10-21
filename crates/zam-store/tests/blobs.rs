//! Блобы вложений: потоковая запись с sha256 и лимитом, дедупликация,
//! счётчик ссылок, разрешение ссылок `_attachments/…` и метаданные для отдачи.

use std::path::PathBuf;

use sha2::{Digest, Sha256};
use zam_core::Id;
use zam_store::blobs::{
    AttachScope, BlobStore, attach, blob_meta, blob_meta_in_space, resolve_attachment,
};
use zam_store::{
    Attribution, AuthorKind, DataDir, Db, NewNote, NodePath, SpaceKind, StoreError, UserDbs,
    create_note, list_spaces,
};

fn datadir(name: &str) -> DataDir {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("blobs-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    DataDir::open(&dir).unwrap()
}

fn hex(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

async fn note(db: &Db, space: Id, path: &str) -> Id {
    create_note(
        db,
        NewNote {
            space_id: space,
            path: NodePath::parse(path).unwrap(),
            content: "x".into(),
            l0: None,
            attrib: Attribution {
                author_kind: AuthorKind::Human,
                token_id: None,
                session_id: None,
                machine: None,
                message: None,
                user_id: None,
            },
        },
    )
    .await
    .unwrap()
    .id
}

/// База пользователя, его хранилище блобов, id Knowledge и Human.
async fn setup(name: &str) -> (DataDir, Db, BlobStore, Id, Id) {
    let dd = datadir(name);
    let user = Id::new();
    let db = UserDbs::new(dd.users_dir()).get(user).await.unwrap();
    let spaces = list_spaces(&db).await.unwrap();
    let find = |k: SpaceKind| spaces.iter().find(|s| s.kind == k).unwrap().id;
    let (k, h) = (find(SpaceKind::Knowledge), find(SpaceKind::Human));
    let bs = BlobStore::new(&dd.blobs_dir(), user);
    (dd, db, bs, k, h)
}

async fn refcount(db: &Db, sha: &str) -> i64 {
    let sha = sha.to_string();
    db.read(move |c| {
        Ok(
            c.query_row("SELECT refcount FROM blobs WHERE sha256 = ?1", [sha], |r| {
                r.get(0)
            })?,
        )
    })
    .await
    .unwrap()
}

fn incoming_is_empty(bs: &BlobStore) -> bool {
    let dir = bs.root().join(".incoming");
    !dir.exists() || std::fs::read_dir(dir).unwrap().next().is_none()
}

#[tokio::test]
async fn put_one_megabyte_hashes_and_places_file() {
    let (dd, _db, bs, _, _) = setup("put").await;
    let data: Vec<u8> = (0..1024 * 1024).map(|i| (i * 31 % 251) as u8).collect();
    let (sha, size) = bs.put(&data[..], 2 * 1024 * 1024).await.unwrap();
    assert_eq!(sha, hex(&data));
    assert_eq!(size, data.len() as u64);
    let p = bs.path_of(&sha);
    assert!(p.starts_with(dd.blobs_dir()));
    assert_eq!(p.parent().unwrap().file_name().unwrap(), &sha[..2]);
    assert_eq!(std::fs::read(&p).unwrap(), data);
    assert!(incoming_is_empty(&bs));
}

#[tokio::test]
async fn same_content_is_one_file_and_refcount_counts_new_pairs_only() {
    let (_dd, db, bs, k, _) = setup("dedup").await;
    let a = note(&db, k, "a").await;
    let b = note(&db, k, "b").await;
    let data = b"\x89PNG same bytes";
    let (sha, size) = bs.put(&data[..], 1000).await.unwrap();
    let (sha2, _) = bs.put(&data[..], 1000).await.unwrap();
    assert_eq!(sha, sha2);
    let files = std::fs::read_dir(bs.path_of(&sha).parent().unwrap())
        .unwrap()
        .count();
    assert_eq!(files, 1);
    assert!(incoming_is_empty(&bs));

    let link = attach(&db, a, &sha, size, "cat.png", "image/png")
        .await
        .unwrap();
    assert_eq!(link, format!("_attachments/{}/cat.png", &sha[..16]));
    assert_eq!(refcount(&db, &sha).await, 1);
    // та же пара — счётчик не растёт
    attach(&db, a, &sha, size, "cat.png", "image/png")
        .await
        .unwrap();
    assert_eq!(refcount(&db, &sha).await, 1);
    // новая пара: другой узел или другое имя
    attach(&db, b, &sha, size, "cat.png", "image/png")
        .await
        .unwrap();
    attach(&db, a, &sha, size, "кот 2.png", "image/png")
        .await
        .unwrap();
    assert_eq!(refcount(&db, &sha).await, 3);
}

#[tokio::test]
async fn over_limit_fails_and_leaves_no_part_file() {
    let (_dd, _db, bs, _, _) = setup("limit").await;
    let data = vec![7u8; 10_001];
    let e = bs.put(&data[..], 10_000).await.unwrap_err();
    assert!(
        matches!(e, StoreError::BadInput(ref m) if m == "too large"),
        "{e}"
    );
    assert!(incoming_is_empty(&bs));
    // ровно на пределе — проходит
    bs.put(&data[..10_000], 10_000).await.unwrap();
    assert!(incoming_is_empty(&bs));
}

#[tokio::test]
async fn link_encodes_markdown_breaking_characters() {
    let (_dd, db, bs, k, _) = setup("link").await;
    let a = note(&db, k, "a").await;
    let (sha, size) = bs.put(&b"x"[..], 10).await.unwrap();
    let link = attach(&db, a, &sha, size, "мой (1) <a>.png", "image/png")
        .await
        .unwrap();
    assert_eq!(
        link,
        format!("_attachments/{}/мой%20%281%29%20%3Ca%3E.png", &sha[..16])
    );
    for bad in ["", "a/b", "..", "a\\b", "x\u{1}"] {
        assert!(
            matches!(
                attach(&db, a, &sha, size, bad, "x/y").await,
                Err(StoreError::BadInput(_))
            ),
            "{bad:?}"
        );
    }
    assert!(matches!(
        attach(&db, a, "nothex", size, "a.png", "image/png").await,
        Err(StoreError::BadInput(_))
    ));
    assert!(matches!(
        attach(&db, Id::new(), &sha, size, "a.png", "image/png").await,
        Err(StoreError::NotFound)
    ));
}

#[tokio::test]
async fn resolve_and_meta_respect_human() {
    let (_dd, db, bs, k, h) = setup("resolve").await;
    let kn = note(&db, k, "k").await;
    let hn = note(&db, h, "h").await;
    let (s1, n1) = bs.put(&b"knowledge file"[..], 100).await.unwrap();
    let (s2, n2) = bs.put(&b"human file"[..], 100).await.unwrap();
    attach(&db, kn, &s1, n1, "a.png", "image/png")
        .await
        .unwrap();
    attach(&db, hn, &s2, n2, "b.pdf", "application/pdf")
        .await
        .unwrap();

    let token = AttachScope {
        prefer_space: Some(k),
        allow_human: false,
        only_space: false,
    };
    let admin = AttachScope {
        prefer_space: None,
        allow_human: true,
        only_space: false,
    };
    // sha16 без учёта регистра
    let up = s1[..16].to_ascii_uppercase();
    assert_eq!(
        resolve_attachment(&db, &up, "a.png", token).await.unwrap(),
        Some((s1.clone(), "image/png".to_string()))
    );
    assert_eq!(
        resolve_attachment(&db, &s2[..16], "b.pdf", token)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        resolve_attachment(&db, &s2[..16], "b.pdf", admin)
            .await
            .unwrap(),
        Some((s2.clone(), "application/pdf".to_string()))
    );
    assert_eq!(
        resolve_attachment(&db, &s1[..16], "другое.png", admin)
            .await
            .unwrap(),
        None
    );

    assert_eq!(
        blob_meta(&db, &s1, "a.png", false).await.unwrap(),
        Some(("image/png".to_string(), n1))
    );
    assert_eq!(blob_meta(&db, &s2, "b.pdf", false).await.unwrap(), None);
    assert_eq!(
        blob_meta(&db, &s2, "b.pdf", true).await.unwrap(),
        Some(("application/pdf".to_string(), n2))
    );
    assert_eq!(blob_meta(&db, &s1, "b.pdf", true).await.unwrap(), None);

    // участник общего решения: только пространство решения
    let only_h = AttachScope {
        prefer_space: Some(h),
        allow_human: true,
        only_space: true,
    };
    assert_eq!(
        resolve_attachment(&db, &s1[..16], "a.png", only_h)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        resolve_attachment(&db, &s2[..16], "b.pdf", only_h)
            .await
            .unwrap(),
        Some((s2.clone(), "application/pdf".to_string()))
    );
    assert_eq!(
        blob_meta_in_space(&db, &s1, "a.png", k).await.unwrap(),
        Some(("image/png".to_string(), n1))
    );
    assert_eq!(
        blob_meta_in_space(&db, &s1, "a.png", h).await.unwrap(),
        None
    );
}
