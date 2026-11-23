//! 可重复的 UI 容量和交互 CPU 测量，不启动 provider。

use std::path::Path;
use std::sync::Arc;

use gpui::{AppContext as _, HeadlessAppContext, size};
use smelt_plugin_api::model::theme_index::DEFAULT_DARK;

use super::snapshots::{EXPORT, demo_rooms};
use super::*;

pub(super) fn capacity_room(seed: &Room, count: usize, activity: Activity) -> Room {
    let mut room = seed.clone();
    room.workers = (0..count)
        .map(|index| {
            let mut worker = seed.workers[index % seed.workers.len()].clone();
            worker.conversation = format!("capacity-{index}");
            worker.title = format!("Capacity agent {index:04}");
            worker.activity = activity;
            worker.completed = false;
            worker
        })
        .collect();
    room
}

/// 测量与本机体验包相同的开发构建；只造显示名册，不创建真实会话或调用模型。
/// CPU 时间包含状态取样、元素构建、布局及 paint 场景生成；不用离屏 GPU renderer，
/// 避免测试平台自动 present 时的 GPU 同步开销污染主线程测量。
#[test]
#[ignore = "manual native capacity measurement"]
fn office_capacity_benchmark() {
    use std::time::{Duration, Instant};

    let _export = EXPORT.lock().unwrap();
    let seed = demo_rooms().remove(0);
    let mut results = Vec::new();
    for activity in [Activity::Idle, Activity::Executing] {
        for count in [12, 50, 100, 200, 500, 1000] {
            let mut cx = HeadlessAppContext::with_platform(
                Arc::new(gpui_apple::AppleTextSystem::new()),
                Arc::new(api::ui::assets::Assets),
                || Ok(None),
            );
            cx.update(|cx| {
                gpui_base::init(cx);
                api::ui::init(cx);
                assert!(api::apply_theme(DEFAULT_DARK, cx));
            });
            let room = capacity_room(&seed, count, activity);
            let epoch = cx.background_executor.now();
            let office = cx
                .new(|_| Office::preview(vec![room], epoch, Some(epoch + Duration::from_secs(4))));
            let window = cx
                .open_window(size(px(1080.), px(800.)), |window, cx| {
                    cx.new(|cx| gpui_base::Root::new(office.clone(), window, cx))
                })
                .unwrap();
            let mut samples = Vec::new();
            for index in 0..30 {
                office.update(&mut cx, |office, cx| {
                    office.preview_time =
                        Some(epoch + Duration::from_secs_f64(4. + index as f64 / 12.));
                    cx.notify();
                });
                cx.run_until_parked();
                let elapsed = cx
                    .update_window(window.into(), |_, window, cx| {
                        let start = Instant::now();
                        window.refresh();
                        window.draw(cx).clear(cx);
                        start.elapsed().as_secs_f64() * 1000.
                    })
                    .unwrap();
                if index >= 5 {
                    samples.push(elapsed);
                }
            }
            cx.update(|cx| assert_eq!(office.read(cx).rooms[0].workers.len(), count));
            samples.sort_by(f64::total_cmp);
            if count == 100 {
                assert!(
                    samples[23] < 16.7,
                    "100 对话 CPU P95 超过 60fps 预算：{}ms",
                    samples[23]
                );
            }
            let result = api::serde_json::json!({
                "conversations": count,
                "activity": format!("{activity:?}"),
                "cpu_p50_ms": samples[12],
                "cpu_p95_ms": samples[23],
                "cpu_max_ms": samples[24],
                "samples": samples.len(),
                "constructed_workers": cx.update(|cx| office.read(cx).built_workers.get()),
            });
            println!("{result}");
            results.push(result);
        }
    }
    let out = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../target/tmp/snapshots/office/capacity.json");
    std::fs::create_dir_all(out.parent().unwrap()).unwrap();
    std::fs::write(&out, api::serde_json::to_vec_pretty(&results).unwrap()).unwrap();
    println!("CPU capacity results: {}", out.display());
}

#[test]
#[ignore = "manual scrolling and status-change measurement"]
fn office_interaction_benchmark() {
    use std::time::{Duration, Instant};

    let _export = EXPORT.lock().unwrap();
    let mut cx = HeadlessAppContext::with_platform(
        Arc::new(gpui_apple::AppleTextSystem::new()),
        Arc::new(api::ui::assets::Assets),
        || Ok(None),
    );
    cx.update(|cx| {
        gpui_base::init(cx);
        api::ui::init(cx);
        assert!(api::apply_theme(DEFAULT_DARK, cx));
    });
    let epoch = cx.background_executor.now();
    let room = capacity_room(&demo_rooms()[0], 100, Activity::Executing);
    let office =
        cx.new(|_| Office::preview(vec![room], epoch, Some(epoch + Duration::from_secs(4))));
    let window = cx
        .open_window(size(px(1080.), px(800.)), |window, cx| {
            cx.new(|cx| gpui_base::Root::new(office.clone(), window, cx))
        })
        .unwrap();
    let mut samples = Vec::new();
    let mut largest_worker_count = 0;
    let mut last_reached = 0;
    for frame in 0..105 {
        cx.run_until_parked();
        let now = epoch + Duration::from_secs_f64(4. + frame as f64 / 60.);
        let elapsed = cx
            .update_window(window.into(), |_, window, cx| {
                let start = Instant::now();
                office.update(cx, |office, cx| {
                    office.preview_time = Some(now);
                    if frame >= 5 {
                        office
                            .list
                            .scroll_by(px(if frame < 80 { 70. } else { -70. }));
                    }
                    if frame % 10 == 0 {
                        for (index, worker) in office.rooms[0].workers.iter_mut().enumerate() {
                            worker.activity = if (index + frame) % 3 == 0 {
                                Activity::Thinking
                            } else {
                                Activity::Executing
                            };
                        }
                        office.cast.sync(&office.rooms, now);
                    }
                    cx.notify();
                });
                window.refresh();
                window.draw(cx).clear(cx);
                start.elapsed().as_secs_f64() * 1000.
            })
            .unwrap();
        cx.update(|cx| {
            let office = office.read(cx);
            largest_worker_count = largest_worker_count.max(office.built_workers.get());
            last_reached = last_reached.max(office.list.logical_scroll_top().item_ix);
        });
        if frame >= 5 {
            samples.push(elapsed);
        }
    }
    assert!(largest_worker_count <= 32);
    assert!(
        last_reached >= 22,
        "必须实际滚到最后几行，实际为 {last_reached}"
    );
    samples.sort_by(f64::total_cmp);
    assert!(
        samples[94] < 16.7,
        "滚动和状态变化时 CPU P95 超过 60fps 预算：{}ms",
        samples[94]
    );
    let result = api::serde_json::json!({
        "conversations":100, "scroll_frames":samples.len(), "cpu_p50_ms":samples[50],
        "cpu_p95_ms":samples[94], "cpu_max_ms":samples[99],
        "max_constructed_workers": largest_worker_count, "last_row_reached":last_reached,
    });
    let out = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../target/tmp/snapshots/office/interaction.json");
    std::fs::create_dir_all(out.parent().unwrap()).unwrap();
    std::fs::write(out, api::serde_json::to_vec_pretty(&result).unwrap()).unwrap();
    println!("{result}");
}
