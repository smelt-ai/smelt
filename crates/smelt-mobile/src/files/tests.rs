//! 项目文件：调用的线格式、子项路径按那台机器的规则拼、四种预览、搜索结果投影。用
//! `RuntimeStream::pair()` 上只认 control 的假运行时。

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::sync::Mutex;

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
fn listing_joins_child_paths_with_the_machines_rules() {
    let runtime = FakeRuntime::new();
    runtime.result(
        "project.tree",
        json!({ "entries": [
            { "name": "src", "is_dir": true },
            { "name": "README.md", "is_dir": false },
        ]}),
    );
    let files = connected(&runtime);

    let posix = files
        .list_dir("/home/u/app", "/home/u/app", PathStyle::Posix)
        .unwrap();
    assert_eq!(
        posix,
        vec![
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
        runtime.params("project.tree"),
        vec![json!({ "root": "/home/u/app", "dir": "/home/u/app" })]
    );

    let windows = files
        .list_dir(r"C:\app", r"C:\app\src", PathStyle::Windows)
        .unwrap();
    assert_eq!(windows[1].path, r"C:\app\src\README.md");
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
        .list_dir("/p", "/p", PathStyle::Posix)
        .unwrap_err();
    assert_eq!(error.kind, SmeltErrorKind::RuntimeOutdated);
}
