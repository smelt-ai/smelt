//! 本机库单测：迁移、机器登记的增删改、草稿（含图片顺序）。

use super::*;

fn info(runtime_id: &str, endpoint_id: &str, hostname: Option<&str>) -> RuntimeInfo {
    RuntimeInfo {
        runtime_id: runtime_id.to_string(),
        hostname: hostname.map(str::to_string),
        os: Some("macos".to_string()),
        version: "1.0.0".to_string(),
        endpoint_id: endpoint_id.to_string(),
        relay_url: None,
    }
}

fn ids(store: &Store) -> Vec<String> {
    store
        .runtimes()
        .unwrap()
        .into_iter()
        .map(|record| record.runtime_id)
        .collect()
}

fn draft(content: &str, images: &[(&str, &[u8])], request_id: Option<&str>) -> MessageDraft {
    MessageDraft {
        content: content.to_string(),
        images: images
            .iter()
            .map(|(mime, bytes)| ImageInput {
                mime: mime.to_string(),
                bytes: bytes.to_vec(),
            })
            .collect(),
        request_id: request_id.map(str::to_string),
    }
}

fn temp_path(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "smelt-mobile-store-{name}-{}-{}",
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(DATABASE_FILE)
}

// ---------------------------------------------------------------------------
// 迁移
// ---------------------------------------------------------------------------

#[test]
fn a_new_database_is_migrated_to_the_latest_version() {
    let path = temp_path("new");
    let store = Store::open(&path).unwrap();
    store.record_pairing(&info("r1", "e1", None), 1).unwrap();
    drop(store);

    let conn = Connection::open(&path).unwrap();
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, MIGRATIONS.len() as i64);
    drop(conn);
    // 再开一次不重建、不丢数据。
    assert_eq!(ids(&Store::open(&path).unwrap()), ["r1"]);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn migrations_run_step_by_step_and_keep_the_data() {
    let mut conn = Connection::open_in_memory().unwrap();
    migrate(&mut conn, &["CREATE TABLE t (a INTEGER NOT NULL);"]).unwrap();
    conn.execute("INSERT INTO t (a) VALUES (7)", []).unwrap();

    let steps = [
        "CREATE TABLE t (a INTEGER NOT NULL);",
        "ALTER TABLE t ADD COLUMN b TEXT NOT NULL DEFAULT 'x';",
        "UPDATE t SET b = 'y';",
    ];
    migrate(&mut conn, &steps).unwrap();

    let (a, b): (i64, String) = conn
        .query_row("SELECT a, b FROM t", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    assert_eq!((a, b.as_str()), (7, "y"), "只跑没跑过的步骤，旧数据还在");
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 3);
}

#[test]
fn a_failed_step_keeps_the_previous_version() {
    let mut conn = Connection::open_in_memory().unwrap();
    let steps = ["CREATE TABLE t (a INTEGER);", "这不是 SQL;"];
    assert!(migrate(&mut conn, &steps).is_err());
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 1, "第一步已提交，第二步回滚");
}

#[test]
fn a_newer_database_is_refused_and_left_alone() {
    let path = temp_path("newer");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("CREATE TABLE keep (a INTEGER); INSERT INTO keep VALUES (1);")
            .unwrap();
        conn.pragma_update(None, "user_version", 99).unwrap();
    }
    assert!(Store::open(&path).is_err());
    let conn = Connection::open(&path).unwrap();
    let kept: i64 = conn
        .query_row("SELECT a FROM keep", [], |row| row.get(0))
        .unwrap();
    assert_eq!(kept, 1, "不删库");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

// ---------------------------------------------------------------------------
// 机器
// ---------------------------------------------------------------------------

#[test]
fn pairing_registers_new_runtimes_with_defaults() {
    let store = Store::in_memory();
    let a = store
        .record_pairing(&info("r1", "endpoint-a-123456", Some(" mac-mini ")), 10)
        .unwrap();
    assert_eq!(a.replaced, None);
    let a = a.record;
    assert_eq!(a.name, "mac-mini", "默认名字取主机名");
    assert_eq!((a.accent, a.position, a.paired_at_ms), (0, 0, 10));

    let b = store
        .record_pairing(&info("r2", "endpoint-b-123456", None), 11)
        .unwrap()
        .record;
    assert_eq!(b.name, "Runtime endpoint", "没有主机名用 endpoint 前缀");
    assert_eq!((b.accent, b.position), (1, 1));
    assert_eq!(ids(&store), ["r1", "r2"]);
}

#[test]
fn pairing_again_updates_the_address_and_keeps_user_settings() {
    let store = Store::in_memory();
    store
        .record_pairing(&info("r1", "e1", Some("host")), 1)
        .unwrap();
    store
        .record_pairing(&info("r2", "e2", Some("other")), 2)
        .unwrap();
    store.rename("r1", "工作机").unwrap();
    store.set_accent("r1", 5).unwrap();

    let mut moved = info("r1", "e1-new", Some("host2"));
    moved.relay_url = Some("https://relay.example/".to_string());
    moved.version = "2.0.0".to_string();
    let paired = store.record_pairing(&moved, 3).unwrap();
    assert_eq!(paired.replaced, None);
    let record = paired.record;
    assert_eq!(record.endpoint_id, "e1-new");
    assert_eq!(record.relay_url.as_deref(), Some("https://relay.example/"));
    assert_eq!(record.hostname.as_deref(), Some("host2"));
    assert_eq!(record.version, "2.0.0");
    assert_eq!(record.paired_at_ms, 3);
    assert_eq!(
        (record.name.as_str(), record.accent, record.position),
        ("工作机", 5, 0),
        "名字、标签色、顺序不变"
    );
    assert_eq!(store.runtimes().unwrap().len(), 2, "更新而不是新增");
}

#[test]
fn an_endpoint_that_now_reports_another_runtime_replaces_the_old_entry() {
    let store = Store::in_memory();
    store.record_pairing(&info("old", "e1", None), 1).unwrap();
    store
        .save_draft("old", "s1", &draft("留着吗", &[], None), 1)
        .unwrap();
    let paired = store.record_pairing(&info("new", "e1", None), 2).unwrap();
    assert_eq!(paired.replaced.as_deref(), Some("old"));
    assert_eq!(ids(&store), ["new"]);
    assert_eq!(paired.record.position, 0);
    assert!(store.load_draft("old", "s1").unwrap().is_none());
}

#[test]
fn accents_take_the_least_used_smallest_index() {
    assert_eq!(next_accent(&[]), 0);
    assert_eq!(next_accent(&[0, 1, 3]), 2);
    let full: Vec<i64> = (0..ACCENT_COUNT as i64).collect();
    assert_eq!(next_accent(&full), 0, "用满了从头再来");
    let mut more = full.clone();
    more.extend([0, 1]);
    assert_eq!(next_accent(&more), 2);

    let store = Store::in_memory();
    store.record_pairing(&info("r1", "e1", None), 1).unwrap();
    store.record_pairing(&info("r2", "e2", None), 2).unwrap();
    store.delete_runtime("r1").unwrap();
    let r3 = store.record_pairing(&info("r3", "e3", None), 3).unwrap();
    assert_eq!(r3.record.accent, 0, "被忘记的机器空出的色号先用");
}

#[test]
fn rename_and_accent_validate_their_input() {
    let store = Store::in_memory();
    store.record_pairing(&info("r1", "e1", None), 1).unwrap();
    assert!(store.rename("r1", "  ").is_err());
    assert!(store.rename("nope", "x").is_err());
    assert!(store.set_accent("r1", ACCENT_COUNT).is_err());
    store.rename("r1", "  书房  ").unwrap();
    assert_eq!(store.runtimes().unwrap()[0].name, "书房");
}

#[test]
fn moving_reorders_and_keeps_positions_contiguous() {
    let store = Store::in_memory();
    for (i, id) in ["a", "b", "c", "d"].iter().enumerate() {
        store
            .record_pairing(&info(id, &format!("e-{id}"), None), i as i64)
            .unwrap();
    }
    store.move_runtime("d", 0).unwrap();
    assert_eq!(ids(&store), ["d", "a", "b", "c"]);
    store.move_runtime("d", 99).unwrap();
    assert_eq!(ids(&store), ["a", "b", "c", "d"]);
    store.move_runtime("a", 2).unwrap();
    assert_eq!(ids(&store), ["b", "c", "a", "d"]);
    let positions: Vec<u32> = store
        .runtimes()
        .unwrap()
        .iter()
        .map(|record| record.position)
        .collect();
    assert_eq!(positions, [0, 1, 2, 3]);
    assert!(store.move_runtime("nope", 0).is_err());

    store.delete_runtime("c").unwrap();
    assert_eq!(ids(&store), ["b", "a", "d"]);
    let positions: Vec<u32> = store
        .runtimes()
        .unwrap()
        .iter()
        .map(|record| record.position)
        .collect();
    assert_eq!(positions, [0, 1, 2], "删除后顺序仍然连续");
}

#[test]
fn deleting_a_runtime_drops_its_drafts_only() {
    let store = Store::in_memory();
    store.record_pairing(&info("r1", "e1", None), 1).unwrap();
    store.record_pairing(&info("r2", "e2", None), 1).unwrap();
    store
        .save_draft("r1", "s", &draft("a", &[("image/png", b"1")], None), 1)
        .unwrap();
    store
        .save_draft("r2", "s", &draft("b", &[("image/png", b"2")], None), 1)
        .unwrap();

    assert!(store.delete_runtime("r1").unwrap());
    assert!(!store.delete_runtime("r1").unwrap());
    assert!(store.load_draft("r1", "s").unwrap().is_none());
    let images: i64 = store
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM draft_image WHERE runtime_id = 'r1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(images, 0);
    assert_eq!(store.load_draft("r2", "s").unwrap().unwrap().content, "b");
}

#[test]
fn import_skips_known_runtimes_and_refreshes_on_describe() {
    let store = Store::in_memory();
    store.record_pairing(&info("r1", "e1", None), 1).unwrap();
    assert!(
        store
            .import_runtime("r1", "other", None, "x", 2)
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .import_runtime("other", "e1", None, "x", 2)
            .unwrap()
            .is_none()
    );
    let imported = store
        .import_runtime("r2", "e2", Some("relay.example"), "  ", 2)
        .unwrap()
        .unwrap();
    assert_eq!(imported.name, "Runtime e2");
    assert_eq!(imported.version, "");
    assert_eq!((imported.accent, imported.position), (1, 1));

    let described = store
        .record_describe(&info("r2", "e2", Some("box")))
        .unwrap()
        .unwrap();
    assert_eq!(described.hostname.as_deref(), Some("box"));
    assert_eq!(described.version, "1.0.0");
    assert_eq!(described.name, "Runtime e2", "名字不跟着主机名变");
    assert!(
        store
            .record_describe(&info("r2", "e2", Some("box")))
            .unwrap()
            .is_none(),
        "没变化不算更新"
    );
}

// ---------------------------------------------------------------------------
// 草稿
// ---------------------------------------------------------------------------

#[test]
fn drafts_roundtrip_with_images_in_order() {
    let store = Store::in_memory();
    store.record_pairing(&info("r1", "e1", None), 1).unwrap();
    let saved = draft(
        "看这几张",
        &[
            ("image/png", b"first"),
            ("image/jpeg", b"second"),
            ("image/png", b"third"),
        ],
        Some("req-1"),
    );
    store.save_draft("r1", "s1", &saved, 1).unwrap();
    assert_eq!(store.load_draft("r1", "s1").unwrap().unwrap(), saved);
    assert!(store.load_draft("r1", "s2").unwrap().is_none());

    // 整份替换：图变少、换序。
    let replaced = draft("", &[("image/jpeg", b"second")], None);
    store.save_draft("r1", "s1", &replaced, 2).unwrap();
    assert_eq!(store.load_draft("r1", "s1").unwrap().unwrap(), replaced);

    store
        .save_draft("r1", "s1", &draft("  ", &[], None), 3)
        .unwrap();
    assert!(
        store.load_draft("r1", "s1").unwrap().is_none(),
        "空草稿等于删"
    );

    store.save_draft("r1", "s1", &saved, 4).unwrap();
    store.clear_draft("r1", "s1").unwrap();
    assert!(store.load_draft("r1", "s1").unwrap().is_none());
}

#[test]
fn drafts_need_a_registered_runtime() {
    let store = Store::in_memory();
    assert!(
        store
            .save_draft("nope", "s", &draft("x", &[], None), 1)
            .is_err()
    );
}

#[test]
fn resolving_a_send_only_touches_the_same_request() {
    let store = Store::in_memory();
    store.record_pairing(&info("r1", "e1", None), 1).unwrap();
    store
        .save_draft("r1", "s", &draft("hi", &[], Some("req-1")), 1)
        .unwrap();

    assert!(!store.resolve_draft("r1", "s", "req-0", true).unwrap());
    assert!(store.resolve_draft("r1", "s", "req-1", false).unwrap());
    let kept = store.load_draft("r1", "s").unwrap().unwrap();
    assert_eq!(kept.request_id, None, "失败后草稿留着、请求 id 清掉");
    assert_eq!(kept.content, "hi");

    store
        .save_draft("r1", "s", &draft("hi", &[], Some("req-2")), 2)
        .unwrap();
    assert!(store.resolve_draft("r1", "s", "req-2", true).unwrap());
    assert!(store.load_draft("r1", "s").unwrap().is_none());
    assert!(!store.resolve_draft("r1", "s", "req-2", true).unwrap());
}
