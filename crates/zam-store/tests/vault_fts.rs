//! Миграция 0018 (`vault_fts`, M4.5 §8): дозаполнение живых записей Vault из открытых полей.
//! Значение и шифротекст в индекс не попадают; удалённые записи не индексируются.

use std::path::PathBuf;

use rusqlite::Connection;
use zam_core::Id;
use zam_store::{Db, USER_MIGRATIONS, VaultScope, vault_list};

fn tmp_dir(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("vault-fts")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn put(c: &Connection, id: &str, title: &str, data: &str, deleted: bool) {
    c.execute(
        "INSERT INTO vault_items (id, scope, agent_solution_id, title, data, secret, current_rev,
             author_kind, created_at, updated_at, deleted_at)
         VALUES (?1, 'knowledge', NULL, ?2, ?3, x'017a656272610a', 1, 'agent', '2026-10-28T10:00:00Z', '2026-10-28T10:00:00Z', ?4)",
        rusqlite::params![id, title, data, deleted.then_some("2026-10-28T11:00:00Z")],
    )
    .unwrap();
}

#[tokio::test]
async fn vault_fts_backfill_migration() {
    let path = tmp_dir("backfill").join("u.db");
    // база M4.5 до 0018: Vault есть, индекса нет
    {
        let db = Db::open(&path, &USER_MIGRATIONS[..17]).unwrap();
        drop(db);
        let c = Connection::open(&path).unwrap();
        let tables: i64 = c
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = 'vault_fts'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tables, 0);
        let (a, b, d) = (Id::new(), Id::new(), Id::new());
        put(
            &c,
            &a.to_string(),
            "стенд-1",
            r#"{"type":"host","purpose":"интеграционные тесты","address":"10.0.0.5:22","limits":"только ночью","notes":"ротация ключей"}"#,
            false,
        );
        put(
            &c,
            &b.to_string(),
            "github",
            r#"{"type":"token","url":"https://git.example.com","description":"деплой","username":"ci"}"#,
            false,
        );
        put(
            &c,
            &d.to_string(),
            "удалённый",
            r#"{"type":"host","purpose":"призрак"}"#,
            true,
        );
    }
    let db = Db::open(&path, USER_MIGRATIONS).unwrap();
    let rows: Vec<(String, String)> = db
        .read(|c| {
            let mut st = c.prepare("SELECT title, open_text FROM vault_fts ORDER BY title")?;
            Ok(st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<_, _>>()?)
        })
        .await
        .unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0].0, "github");
    assert!(rows[0].1.contains("деплой") && rows[0].1.contains("git.example.com"));
    assert!(rows[0].1.contains("ci"));
    assert!(rows[1].1.contains("интеграционные") && rows[1].1.contains("ротация"));
    assert!(rows[1].1.contains("только ночью") && rows[1].1.contains("10.0.0.5"));
    let all = rows
        .iter()
        .map(|r| format!("{}{}", r.0, r.1))
        .collect::<String>();
    assert!(!all.contains("zebra") && !all.contains("призрак"), "{all}");
    // поиск работает на дозаполненном индексе
    let k = VaultScope::Knowledge;
    let hit = vault_list(&db, &k, Some("ротац")).await.unwrap();
    assert_eq!(hit.len(), 1);
    assert_eq!(hit[0].title, "стенд-1");
    assert!(
        vault_list(&db, &k, Some("призрак"))
            .await
            .unwrap()
            .is_empty()
    );
}
