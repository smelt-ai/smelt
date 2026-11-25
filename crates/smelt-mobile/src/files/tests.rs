//! 项目文件：调用的线格式、子项路径按那台机器的规则拼、四种预览、搜索结果投影。用
//! `RuntimeStream::pair()` 上只认 control 的假运行时。

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::sync::{Mutex, mpsc};
use std::time::Instant;

use serde_json::{Value, json};
use smelt_runtime_client::protocol::control::{ControlError, ControlErrorCode, ControlResponse};
use smelt_runtime_client::protocol::write_json_line;
use smelt_runtime_client::{RuntimeClient, RuntimeStream};

use super::*;
use crate::api::files::FilePreviewKind;

const FAST: Timing = Timing {
    call_timeout: Duration::from_secs(3),
    open_timeout: Duration::from_secs(3),
};

/// 按方法名回固定结果的运行时，记下收到的每个 control 请求。
#[derive(Default)]
struct FakeRuntime {
    results: Mutex<HashMap<String, Value>>,
    calls: Mutex<Vec<(String, Value)>>,
}

impl FakeRuntime {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn result(&self, method: &str, result: Value) {
        self.results
            .lock()
            .unwrap()
            .insert(method.to_string(), result);
    }

    fn clear_result(&self, method: &str) {
        self.results.lock().unwrap().remove(method);
    }

    fn client(self: &Arc<Self>) -> RuntimeClient {
        let runtime = Arc::clone(self);
        RuntimeClient::new(move || {
            let (client, server) = RuntimeStream::pair()?;
            let runtime = Arc::clone(&runtime);
            std::thread::spawn(move || runtime.serve(server));
            Ok(client)
        })
    }

    fn serve(&self, server: RuntimeStream) {
        let mut line = String::new();
        if BufReader::new(&server).read_line(&mut line).is_err() || line.is_empty() {
            return;
        }
        let request: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["op"], "control", "只该发 control 调用");
        let id = request["request_id"].as_str().unwrap();
        let method = request["method"].as_str().unwrap().to_string();
        self.calls
            .lock()
            .unwrap()
            .push((method.clone(), request["params"].clone()));
        let response = match self.results.lock().unwrap().get(&method) {
            Some(result) => serde_json::to_value(ControlResponse::ok(id, result.clone())),
            None => serde_json::to_value(ControlResponse::<()>::error(
                id,
                ControlError::new(ControlErrorCode::MethodNotFound, method.clone()),
            )),
        }
        .unwrap();
        let _ = write_json_line(&mut &server, &response);
    }

    fn params(&self, method: &str) -> Vec<Value> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _)| m == method)
            .map(|(_, params)| params.clone())
            .collect()
    }
}

struct FakeSource(Option<CurrentRuntime>);

impl RuntimeSource for FakeSource {
    fn current(&self) -> Option<CurrentRuntime> {
        self.0.clone()
    }
}

fn connected(runtime: &Arc<FakeRuntime>) -> Files {
    Files::new(
        Arc::new(FakeSource(Some(CurrentRuntime {
            generation: 1,
            endpoint_id: "ep-a".into(),
            client: runtime.client(),
        }))),
        FAST,
    )
}

#[test]
fn listing_hides_noise_sorts_dirs_first_and_joins_paths_with_the_machines_rules() {
    let runtime = FakeRuntime::new();
    let stat = |kind: &str, target: Option<&str>| {
        json!({ "kind": kind, "target_kind": target, "size": 0, "allocated": 0,
                "modified": null, "identity": null })
    };
    let entry = |name: &str, kind: &str, target: Option<&str>| {
        let mut value = stat(kind, target);
        value["name"] = json!(name);
        value
    };
    runtime.result(
        "fs.read_dir",
        json!({ "entries": [
            entry("README.md", "file", None),
            entry(".git", "dir", None),
            entry("src", "dir", None),
            entry("docs", "symlink", Some("dir")),
            entry("node_modules", "dir", None),
        ]}),
    );
    let files = connected(&runtime);

    let posix = files.list_dir("/home/u/app", PathStyle::Posix).unwrap();
    assert_eq!(
        posix,
        vec![
            FileEntry {
                name: "docs".into(),
                path: "/home/u/app/docs".into(),
                is_dir: true
            },
            FileEntry {
                name: "src".into(),
                path: "/home/u/app/src".into(),
                is_dir: true
            },
            FileEntry {
                name: "README.md".into(),
                path: "/home/u/app/README.md".into(),
                is_dir: false,
            },
        ]
    );
    assert_eq!(
        runtime.params("fs.read_dir"),
        vec![json!({ "path": "/home/u/app" })]
    );

    let windows = files.list_dir(r"C:\app\src", PathStyle::Windows).unwrap();
    assert_eq!(windows[2].path, r"C:\app\src\README.md");
}

#[test]
fn opening_maps_each_kind_of_content() {
    let runtime = FakeRuntime::new();
    let files = connected(&runtime);

    runtime.result(
        "file.open",
        json!({ "content": { "kind": "text", "text": "fn a() {}\n", "version": "v1" } }),
    );
    let text = files.open("/p/a.rs").unwrap();
    assert_eq!(text.kind, FilePreviewKind::Text);
    let document = text.document.expect("文本应该成文档");
    assert_eq!(document.line_count(), 1);
    assert_eq!(document.language().as_deref(), Some("Rust"));
    assert_eq!(document.full_text(), "fn a() {}\n");

    // `data` 是 base64：`PNG` → `UE5H`。
    runtime.result(
        "file.open",
        json!({ "content": { "kind": "image", "data": "UE5H" } }),
    );
    let image = files.open("/p/logo.SVG").unwrap();
    assert_eq!(image.kind, FilePreviewKind::Image);
    assert_eq!(image.image.as_deref(), Some(&b"PNG"[..]));
    assert!(image.svg);

    runtime.result("file.open", json!({ "content": { "kind": "binary" } }));
    assert_eq!(
        files.open("/p/a.bin").unwrap().kind,
        FilePreviewKind::Binary
    );

    runtime.result(
        "file.open",
        json!({ "content": { "kind": "too_large", "size": 9_000_000, "limit": 8_388_608 } }),
    );
    let huge = files.open("/p/huge.log").unwrap();
    assert_eq!(huge.kind, FilePreviewKind::TooLarge);
    assert_eq!((huge.size, huge.limit), (Some(9_000_000), Some(8_388_608)));
    assert_eq!(runtime.params("file.open")[0], json!({ "path": "/p/a.rs" }));
}

#[test]
fn search_hits_keep_line_previews_and_blank_queries_skip_the_call() {
    let runtime = FakeRuntime::new();
    runtime.result(
        "project.search",
        json!({
            "hits": [
                { "path": "/p/src/main.rs", "rel": "src/main.rs", "line": null },
                { "path": "/p/README.md", "rel": "README.md", "line": [3, "run main"] },
            ],
            "truncated": true,
        }),
    );
    let files = connected(&runtime);
    let found = files.search("/p", "  main ").unwrap();
    assert!(found.truncated);
    assert_eq!(found.hits[0].line, None);
    assert_eq!(found.hits[1].line, Some(3));
    assert_eq!(found.hits[1].preview.as_deref(), Some("run main"));
    assert_eq!(
        runtime.params("project.search"),
        vec![json!({ "root": "/p", "query": "main" })]
    );

    assert!(files.search("/p", "   ").unwrap().hits.is_empty());
    assert_eq!(runtime.params("project.search").len(), 1);
}

#[test]
fn offline_and_old_runtimes_explain_themselves() {
    let offline = Files::new(Arc::new(FakeSource(None)), FAST);
    assert_eq!(
        offline.open("/p/a").err().unwrap().kind,
        SmeltErrorKind::Offline
    );

    // 老运行时不认这个方法。
    let runtime = FakeRuntime::new();
    let error = connected(&runtime)
        .list_dir("/p", PathStyle::Posix)
        .unwrap_err();
    assert_eq!(error.kind, SmeltErrorKind::RuntimeOutdated);
}

#[test]
fn opening_revalidates_cached_text_and_image_with_if_none_match() {
    let runtime = FakeRuntime::new();
    let files = connected(&runtime);

    runtime.result(
        "file.open",
        json!({ "content": { "kind": "text", "text": "fn a() {}\n", "version": "v1" } }),
    );
    let first = files.open("/p/a.rs").unwrap();
    assert_eq!(first.document.unwrap().full_text(), "fn a() {}\n");

    // 命中缓存发送 `if_none_match`，服务端返回 `NotModified` 时直接沿用缓存文档。
    runtime.result(
        "file.open",
        json!({ "content": { "kind": "not_modified", "version": "v1" } }),
    );
    let cached = files.open("/p/a.rs").unwrap();
    let doc = cached.document.unwrap();
    assert_eq!(doc.full_text(), "fn a() {}\n");
    assert_eq!(doc.language().as_deref(), Some("Rust"));
    assert_eq!(
        runtime.params("file.open")[1],
        json!({ "path": "/p/a.rs", "if_none_match": "v1" })
    );

    // 新版本返回后刷新缓存。
    runtime.result(
        "file.open",
        json!({ "content": { "kind": "text", "text": "fn b() {}\n", "version": "v2" } }),
    );
    assert_eq!(
        files.open("/p/a.rs").unwrap().document.unwrap().full_text(),
        "fn b() {}\n"
    );

    // 带 version 的图片也参与 `if_none_match` 校验与 `NotModified` 复用。
    runtime.result(
        "file.open",
        json!({ "content": { "kind": "image", "data": "UE5H", "version": "img1" } }),
    );
    let img = files.open("/p/logo.SVG").unwrap();
    assert_eq!(img.image.as_deref(), Some(&b"PNG"[..]));
    assert!(img.svg);

    runtime.result(
        "file.open",
        json!({ "content": { "kind": "not_modified", "version": "img1" } }),
    );
    let img_cached = files.open("/p/logo.SVG").unwrap();
    assert_eq!(img_cached.image.as_deref(), Some(&b"PNG"[..]));
    assert!(img_cached.svg);
    assert_eq!(
        runtime.params("file.open")[4],
        json!({ "path": "/p/logo.SVG", "if_none_match": "img1" })
    );

    // 二进制与协议错误都要清理对应路径的过期缓存。
    runtime.result("file.open", json!({ "content": { "kind": "binary" } }));
    assert_eq!(files.open("/p/a.rs").unwrap().kind, FilePreviewKind::Binary);

    runtime.result(
        "file.open",
        json!({ "content": { "kind": "text", "text": "fn c() {}\n", "version": "v3" } }),
    );
    files.open("/p/a.rs").unwrap();
    assert_eq!(runtime.params("file.open")[6], json!({ "path": "/p/a.rs" }));

    runtime.clear_result("file.open");
    assert!(files.open("/p/a.rs").is_err());

    runtime.result(
        "file.open",
        json!({ "content": { "kind": "text", "text": "fn d() {}\n", "version": "v4" } }),
    );
    files.open("/p/a.rs").unwrap();
    assert_eq!(runtime.params("file.open")[8], json!({ "path": "/p/a.rs" }));
}

#[test]
fn cache_evicts_lru_and_skips_oversized_entries() {
    let runtime = FakeRuntime::new();
    let source = Arc::new(FakeSource(Some(CurrentRuntime {
        generation: 1,
        endpoint_id: "ep-a".into(),
        client: runtime.client(),
    })));
    // 总上限 16 字节，单个超过 8 字节不进缓存。
    let files = Files::with_cache_limit(source, FAST, 16);

    runtime.result(
        "file.open",
        json!({ "content": { "kind": "text", "text": "123456", "version": "va" } }),
    );
    files.open("/p/a").unwrap();

    runtime.result(
        "file.open",
        json!({ "content": { "kind": "text", "text": "abcdef", "version": "vb" } }),
    );
    files.open("/p/b").unwrap();

    // 命中 /p/a 刷新其最近使用顺序，使 /p/b 成为最久未用。
    runtime.result(
        "file.open",
        json!({ "content": { "kind": "not_modified", "version": "va" } }),
    );
    files.open("/p/a").unwrap();

    // 再塞 6 字节的 /p/c：6+6+6=18 > 16，淘汰 /p/b，保留 /p/a 和 /p/c。
    runtime.result(
        "file.open",
        json!({ "content": { "kind": "text", "text": "zzzzzz", "version": "vc" } }),
    );
    files.open("/p/c").unwrap();

    runtime.result(
        "file.open",
        json!({ "content": { "kind": "text", "text": "new-b", "version": "vb2" } }),
    );
    files.open("/p/b").unwrap();
    assert_eq!(runtime.params("file.open")[4], json!({ "path": "/p/b" }));

    // /p/a 此时总计 6(a)+6(c)+5(b)=17 -> 上一步已淘汰 /p/a，换小上限再测超限跳过：
    runtime.result(
        "file.open",
        json!({ "content": { "kind": "text", "text": "tiny", "version": "vt" } }),
    );
    files.open("/p/t").unwrap();
    runtime.result(
        "file.open",
        json!({ "content": { "kind": "text", "text": "123456789", "version": "vbig" } }),
    );
    files.open("/p/t").unwrap();
    runtime.result(
        "file.open",
        json!({ "content": { "kind": "text", "text": "after", "version": "vafter" } }),
    );
    files.open("/p/t").unwrap();
    // 超过 8 字节时会清掉旧缓存并跳过写入，再开不带 `if_none_match`。
    assert_eq!(runtime.params("file.open")[7], json!({ "path": "/p/t" }));
}

#[test]
fn newer_open_and_close_cancel_inflight_open() {
    let calls = Arc::new(Mutex::new(Vec::<(String, Option<String>)>::new()));
    let (slow1_tx, slow1_rx) = mpsc::channel();
    let (slow2_tx, slow2_rx) = mpsc::channel();
    let calls_srv = Arc::clone(&calls);

    let client = RuntimeClient::new(move || {
        let (cli, srv) = RuntimeStream::pair()?;
        let calls_srv = Arc::clone(&calls_srv);
        let slow1_tx = slow1_tx.clone();
        let slow2_tx = slow2_tx.clone();
        std::thread::spawn(move || {
            let mut line = String::new();
            let mut reader = BufReader::new(&srv);
            if reader.read_line(&mut line).is_err() || line.is_empty() {
                return;
            }
            let req: Value = serde_json::from_str(&line).unwrap();
            let id = req["request_id"].as_str().unwrap().to_string();
            let path = req["params"]["path"].as_str().unwrap().to_string();
            let if_none_match = req["params"]["if_none_match"].as_str().map(str::to_string);
            calls_srv
                .lock()
                .unwrap()
                .push((path.clone(), if_none_match));
            if path == "/p/slow1" {
                let _ = slow1_tx.send(());
                let mut sink = [0u8; 16];
                while reader.read(&mut sink).unwrap_or(0) > 0 {}
                return;
            }
            if path == "/p/slow2" {
                let _ = slow2_tx.send(());
                let mut sink = [0u8; 16];
                while reader.read(&mut sink).unwrap_or(0) > 0 {}
                return;
            }
            let response = serde_json::to_value(ControlResponse::ok(
                id,
                json!({ "content": { "kind": "text", "text": "fast\n", "version": "vf" } }),
            ))
            .unwrap();
            let _ = write_json_line(&mut &srv, &response);
        });
        Ok(cli)
    });

    let files = Arc::new(Files::new(
        Arc::new(FakeSource(Some(CurrentRuntime {
            generation: 1,
            endpoint_id: "ep-a".into(),
            client,
        }))),
        FAST,
    ));

    // 新打开抢占旧的在途打开，旧请求应远快于 3s 超时立刻返回。
    let files1 = Arc::clone(&files);
    let t1 = std::thread::spawn(move || {
        let start = Instant::now();
        let res = files1.open("/p/slow1");
        (res, start.elapsed())
    });
    slow1_rx.recv_timeout(Duration::from_secs(2)).unwrap();

    let fast = files.open("/p/fast").unwrap();
    assert_eq!(fast.document.unwrap().full_text(), "fast\n");

    let (res1, elapsed1) = t1.join().unwrap();
    assert!(res1.is_err());
    assert!(
        elapsed1 < Duration::from_secs(2),
        "expected superseded open to cancel promptly, took {elapsed1:?}"
    );

    // 节点关闭会中止在途打开并清空缓存。
    let files2 = Arc::clone(&files);
    let t2 = std::thread::spawn(move || {
        let start = Instant::now();
        let res = files2.open("/p/slow2");
        (res, start.elapsed())
    });
    slow2_rx.recv_timeout(Duration::from_secs(2)).unwrap();

    files.close();
    let (res2, elapsed2) = t2.join().unwrap();
    assert!(res2.is_err());
    assert!(
        elapsed2 < Duration::from_secs(2),
        "expected close to cancel inflight open promptly, took {elapsed2:?}"
    );

    // `/p/fast` 的缓存已被 close() 清空，重开不再带 `if_none_match`。
    assert_eq!(
        files.open("/p/fast").unwrap().document.unwrap().full_text(),
        "fast\n"
    );
    let recorded = calls.lock().unwrap();
    assert_eq!(recorded.last(), Some(&("/p/fast".to_string(), None)));
}

#[test]
fn is_svg_matches_windows_and_posix_filenames_only() {
    assert!(is_svg("/a/icon.svg"));
    assert!(is_svg(r"E:\assets\Logo.SVG"));
    assert!(!is_svg(r"E:\dir.svg\readme.txt"));
    assert!(!is_svg(r"E:\dir\.svg"));
}
