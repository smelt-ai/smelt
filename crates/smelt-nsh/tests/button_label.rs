//! `<SmeltButton>` 限宽回归测试（独立进程，避开 `nsh_platform_gpui::rt::JOBS` `OnceLock`）：
//! 按钮被 `maxWidth` 或父级压窄时，文字要在按钮里截断，不能撑出按钮底色。NSH 按 React Native
//! 默认 `flexShrink: 0`，按钮里那层内容行不写可收缩就按文字原长排、两头溢出（对话顶栏吸顶提问胶囊）。

use std::{
    collections::HashMap,
    fs,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use gpui::TestAppContext;
use nsh_core::element::description::SetDescription;
use nsh_platform_gpui::{AppOptions, AppView, Limits};
use serde_json::{Value, json};
use smelt_nsh::plugins::{PluginHost, Plugins};

const SHELL: &str = r#"
import { View } from "nsh";
import { Slot } from "./slots.tsx";

export function Shell() {
  return (
    <View style={{ flex: 1 }}>
      <Slot id="probe" />
    </View>
  );
}
"#;

// 内容行末尾放一个 1px 标记：它的右边缘就是内容行排到哪儿；外层包一层量按钮本身多宽。
const PLUGIN: &str = r#"
import { host, View } from "nsh";
import { SmeltButton } from "smelt/sdk";

const LABEL = "Why does the pinned prompt pill get narrower than its own text while scrolling";

const report = (part: string) => ({ bounds }: any) => {
  host.call("test.layout", { part, x: bounds.origin.x, width: bounds.size.width }).catch(() => {});
};

export default function Probe() {
  return (
    <View style={{ width: 800, height: 100, alignItems: "flex-start" }}>
      <View onLayout={report("button")}>
        <SmeltButton floating small label={LABEL} style={{ maxWidth: 200 }}>
          <View onLayout={report("end")} style={{ width: 1, height: 1 }} />
        </SmeltButton>
      </View>
    </View>
  );
}
"#;

#[derive(Debug, Clone, Copy)]
struct Frame {
    x: f64,
    width: f64,
}

#[gpui::test]
fn smelt_button_label_stays_inside_max_width(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    nsh_platform_gpui::set_ui_sets(vec![
        nsh_base::registry(),
        smelt_nsh::testing::host_registry(),
    ]);

    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("button_label");
    let _ = fs::remove_dir_all(&root);

    let host = PluginHost::new();
    host.set_plugin_limits(Limits {
        memory_bytes: 32 << 20,
        load_budget: Duration::from_millis(250),
        call_budget: Duration::from_millis(120),
        ..Limits::default()
    });

    let frames: Arc<Mutex<HashMap<String, Frame>>> = Arc::default();
    let app_errors: Arc<Mutex<Vec<String>>> = Arc::default();

    host.handle("state", |_plugin, call| {
        call.resolve(json!({
            "revision": 1,
            "locale": "en",
            "theme": smelt_nsh::testing::theme_state(16.),
            "machines_meta": [],
            "sessions_data": [],
        }));
    });
    host.handle("call", |_plugin, call| call.resolve(Value::Null));
    {
        let frames = Arc::clone(&frames);
        host.handle("test.layout", move |_plugin, call| {
            let data = call.data();
            let part = data["part"].as_str().unwrap_or_default().to_owned();
            let frame = Frame {
                x: data["x"].as_f64().unwrap_or(f64::NAN),
                width: data["width"].as_f64().unwrap_or(f64::NAN),
            };
            frames.lock().unwrap().insert(part, frame);
            call.resolve(Value::Null);
        });
    }

    let (view, cx) = cx.add_window_view(|window, cx| {
        AppView::new(
            AppOptions {
                inline_root: true,
                ..AppOptions::default()
            },
            window,
            cx,
        )
    });
    {
        let app_errors = Arc::clone(&app_errors);
        let err_host = host.clone();
        view.update(cx, |view, _| {
            view.on_app_error(move |error| {
                app_errors.lock().unwrap().push(error.to_string());
                err_host.on_app_error(error);
            });
        });
    }

    let ui_sets = vec![
        SetDescription::from(nsh_base::registry()),
        SetDescription::from(smelt_nsh::testing::host_registry()),
    ];
    let mut plugins = Plugins::new(&root.join("stage"), SHELL, ui_sets).unwrap();
    let plugin_dir = root.join("sources/probe");
    fs::create_dir_all(&plugin_dir).unwrap();
    fs::write(plugin_dir.join("main.tsx"), PLUGIN).unwrap();
    plugins
        .install_entry("probe", &plugin_dir, "main.tsx", &[])
        .unwrap();
    let start = plugins.start();
    view.update(cx, |view, _| host.apply(view, start));

    let begin = Instant::now();
    let (button, end) = loop {
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        {
            let frames = frames.lock().unwrap();
            if let (Some(button), Some(end)) = (frames.get("button"), frames.get("end")) {
                break (*button, *end);
            }
        }
        assert!(
            begin.elapsed() < Duration::from_secs(10),
            "等待按钮排版超时；errors={:?}",
            app_errors.lock().unwrap(),
        );
        // 轮询例外：测试平台无引擎帧完成事件；周期 10ms，上限 10s。
        std::thread::sleep(Duration::from_millis(10));
    };

    let errs = app_errors.lock().unwrap();
    assert!(errs.is_empty(), "unexpected app_errors: {errs:?}");
    assert!(button.width <= 200.0, "按钮应被 maxWidth 限住：{button:?}");
    assert!(
        end.x + end.width <= button.width,
        "内容行排出了按钮：按钮宽 {}，内容排到 {}",
        button.width,
        end.x + end.width,
    );
}
