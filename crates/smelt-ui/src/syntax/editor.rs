//! 接到 gpui-base 编辑器上的高亮器：编辑器每次改动交给它，它出样式和折叠段。
//!
//! 打字时先在前台增量解析，限 2ms；超时，或文件大于 256KB 不在前台解析，就等 150ms（连续打字
//! 只解析最后一次）到后台解析，解析完换上新树、刷新折叠。等后台的时候用挪好位置的旧树出样式，
//! 只是新打的字暂时没颜色。后台解析途中又有改动就叫停，不再换上过时的结果。

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui::{Context, HighlightStyle, SharedString, Task, Window};
use gpui_base::input::{
    EditorState, FoldRange, HighlightStyleResolver, InputEdit, InputHighlighter,
    InputHighlighterFactory, Rope,
};

use super::SyntaxTree;

const SYNC_PARSE_BUDGET: Duration = Duration::from_millis(2);
const SYNC_PARSE_MAX_BYTES: usize = 256 * 1024;
/// 防抖：连续打字只在停下这么久之后解析一次。
const PARSE_DEBOUNCE: Duration = Duration::from_millis(150);

/// 装到编辑器状态上（`EditorState::set_highlighter_factory`），编辑器换语言时按语言名建高亮器。
/// 没有语法的语言也建：只记文本，不出样式也不出折叠。
pub fn highlighter_factory() -> InputHighlighterFactory {
    Rc::new(|language| Some(Box::new(Highlighter::new(language)) as Box<dyn InputHighlighter>))
}

struct Highlighter {
    language: SharedString,
    /// 后台解析完会整个换掉，所以和后台任务共用一份。
    tree: Rc<RefCell<SyntaxTree>>,
    background: Option<Background>,
}

/// 等着的后台解析。丢掉时叫停：任务不再往下走，已经在解析的也尽快停。
struct Background {
    stop: Arc<AtomicBool>,
    _task: Task<()>,
}

impl Drop for Background {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Highlighter {
    fn new(language: &str) -> Self {
        Self {
            language: language.to_string().into(),
            tree: Rc::new(RefCell::new(SyntaxTree::new(language))),
            background: None,
        }
    }

    /// 等一会儿到后台把当前文本解析完，换上新树、刷新折叠。
    fn parse_in_background(
        &mut self,
        folding: bool,
        window: &mut Window,
        cx: &mut Context<EditorState>,
    ) {
        let stop = Arc::new(AtomicBool::new(false));
        let shared = self.tree.clone();
        let flag = stop.clone();
        let task = cx.spawn_in(window, async move |editor, cx| {
            cx.background_executor().timer(PARSE_DEBOUNCE).await;
            let mut tree = shared.borrow().clone();
            let parsed = cx
                .background_executor()
                .spawn(async move {
                    let text = tree.text().clone();
                    let done = tree.parse(&text, &mut || flag.load(Ordering::Relaxed));
                    let folds = if done && folding {
                        tree.fold_ranges(0..usize::MAX)
                    } else {
                        Vec::new()
                    };
                    done.then_some((tree, folds))
                })
                .await;
            // 叫停了（又有改动）就什么都不换：新改动会排自己的解析。
            let Some((tree, folds)) = parsed else {
                return;
            };
            *shared.borrow_mut() = tree;
            editor
                .update(cx, |editor, cx| {
                    editor.apply_highlighter_fold_candidates(folds, cx)
                })
                .ok();
        });
        self.background = Some(Background { stop, _task: task });
    }
}

impl InputHighlighter for Highlighter {
    fn language(&self) -> SharedString {
        self.language.clone()
    }

    fn update(
        &mut self,
        edit: Option<InputEdit>,
        text: &Rope,
        folding: bool,
        window: &mut Window,
        cx: &mut Context<EditorState>,
    ) {
        // 新改动一来，等着的后台解析就过时了。
        self.background = None;
        let done = {
            let mut tree = self.tree.borrow_mut();
            match edit {
                Some(edit) => tree.edit(&tree_sitter_edit(edit)),
                None => tree.reset(),
            }
            if text.len() > SYNC_PARSE_MAX_BYTES {
                tree.defer(text);
                false
            } else {
                let deadline = Instant::now() + SYNC_PARSE_BUDGET;
                tree.parse(text, &mut || Instant::now() >= deadline)
            }
        };
        if !done {
            self.parse_in_background(folding, window, cx);
        }
    }

    /// 多光标一次改了几处：前几处只挪树，最后一处再解析，整次改动只解析一遍。
    fn update_batch(
        &mut self,
        edits: &[(InputEdit, Rope)],
        folding: bool,
        window: &mut Window,
        cx: &mut Context<EditorState>,
    ) {
        let Some(((last, text), earlier)) = edits.split_last() else {
            return;
        };
        {
            let mut tree = self.tree.borrow_mut();
            for (edit, _) in earlier {
                tree.edit(&tree_sitter_edit(*edit));
            }
        }
        self.update(Some(*last), text, folding, window, cx);
    }

    fn styles(
        &self,
        range: &Range<usize>,
        resolver: &dyn HighlightStyleResolver,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        self.tree.borrow().styles(range.clone(), resolver)
    }

    fn fold_ranges(&self, _: &Rope) -> Vec<FoldRange> {
        self.tree.borrow().fold_ranges(0..usize::MAX)
    }

    fn fold_ranges_for_edit(&self, range: Range<usize>, _: &Rope) -> Vec<FoldRange> {
        self.tree.borrow().fold_ranges(range)
    }
}

fn tree_sitter_edit(edit: InputEdit) -> tree_sitter::InputEdit {
    let point = |p: gpui_base::input::Point| tree_sitter::Point::new(p.row, p.column);
    tree_sitter::InputEdit {
        start_byte: edit.start_byte,
        old_end_byte: edit.old_end_byte,
        new_end_byte: edit.new_end_byte,
        start_position: point(edit.start_position),
        old_end_position: point(edit.old_end_position),
        new_end_position: point(edit.new_end_position),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_keep_byte_and_point_positions() {
        let edit = InputEdit {
            start_byte: 3,
            old_end_byte: 5,
            new_end_byte: 9,
            start_position: gpui_base::input::Point::new(0, 3),
            old_end_position: gpui_base::input::Point::new(0, 5),
            new_end_position: gpui_base::input::Point::new(1, 2),
        };
        let edit = tree_sitter_edit(edit);
        assert_eq!(
            (edit.start_byte, edit.old_end_byte, edit.new_end_byte),
            (3, 5, 9)
        );
        assert_eq!(edit.start_position, tree_sitter::Point::new(0, 3));
        assert_eq!(edit.old_end_position, tree_sitter::Point::new(0, 5));
        assert_eq!(edit.new_end_position, tree_sitter::Point::new(1, 2));
    }
}
