//! 在异步任务里等一个全局状态到位：先看一眼，没到就挂在这个全局的变更通知上，每次变了
//! 再看。代替「睡一会儿再查」——等的时候不醒，状态一变当场醒。

use gpui::{App, AsyncApp, Global};

/// 等 `ready` 在全局 `G` 上给出结果。已经有结果就立刻返回；否则 `G` 每变一次再问一次
/// `ready`，第一次给出 `Some` 就返回它。应用退出、通知没了时返回 `None`。
pub async fn wait_for_global<G: Global, T: 'static>(
    cx: &AsyncApp,
    mut ready: impl FnMut(&App) -> Option<T> + 'static,
) -> Option<T> {
    let (tx, rx) = smol::channel::bounded(1);
    let _watch = cx.update(|cx| {
        if let Some(value) = ready(&*cx) {
            let _ = tx.try_send(value);
            return None;
        }
        Some(cx.observe_global::<G>(move |cx| {
            if let Some(value) = ready(&*cx) {
                let _ = tx.try_send(value);
            }
        }))
    });
    rx.recv().await.ok()
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use gpui::TestAppContext;

    use super::*;

    struct Counter(u32);

    impl Global for Counter {}

    /// 在 `cx` 上起一条等 `Counter` 数到 `target` 的任务，返回它等到的结果（还没返回是 `None`）。
    fn wait_until(target: u32, cx: &mut TestAppContext) -> Rc<Cell<Option<Option<u32>>>> {
        let seen = Rc::new(Cell::new(None));
        let result = Rc::clone(&seen);
        cx.update(|cx| {
            cx.spawn(async move |cx| {
                let value = wait_for_global::<Counter, u32>(cx, move |cx| {
                    let count = cx.global::<Counter>().0;
                    (count >= target).then_some(count)
                })
                .await;
                result.set(Some(value));
            })
            .detach();
        });
        seen
    }

    #[gpui::test]
    fn returns_at_once_when_already_there(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_global(Counter(3)));
        let seen = wait_until(2, cx);
        cx.run_until_parked();
        assert_eq!(seen.get(), Some(Some(3)));
    }

    #[gpui::test]
    fn wakes_on_the_change_that_gets_there(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_global(Counter(0)));
        let seen = wait_until(2, cx);
        cx.run_until_parked();
        assert_eq!(seen.get(), None, "还没到就一直等着");
        cx.update(|cx| cx.set_global(Counter(1)));
        cx.run_until_parked();
        assert_eq!(seen.get(), None, "变了但还不够，接着等");
        cx.update(|cx| cx.global_mut::<Counter>().0 = 2);
        cx.run_until_parked();
        assert_eq!(seen.get(), Some(Some(2)));
    }
}
