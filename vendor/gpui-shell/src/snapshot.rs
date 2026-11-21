//! What a script render produces, and what many GPUI frames consume.
//!
//! A GPUI render is not a script render. GPUI repaints for reasons the script
//! knows nothing about — a cursor blink, a hover, a scroll, an animation frame —
//! and none of those are a reason to enter the VM. So a script `render()` no
//! longer describes *this frame*; it describes the current interface, once, into
//! a [`RenderSnapshot`] that stays valid until script state says otherwise.
//!
//! ```text
//! script state changes ──▶ build snapshot ──▶ ┌───────────┐
//!                                             │ snapshot  │
//!                                             └───────────┘
//!                                                  │  │  │
//!                          many GPUI frames ◀──────┘  │  │
//!                                       materialize ◀─┘  │
//!                                              (no VM) ◀─┘
//! ```
//!
//! The snapshot owns everything materialization needs: the element descriptions,
//! the root, and — indirectly, through its generation — the handlers the script
//! registered while building it. That ownership is the point. When the snapshot
//! is dropped its callbacks are retired with it, which is what lets several
//! views share one runtime without one view's render invalidating another's
//! buttons.
//!
//! # Where the diagram is not the whole truth
//!
//! One component breaks the right-hand column: a virtualized list. Its rows
//! depend on where it has been scrolled, which GPUI only knows during layout,
//! so its item renderer is called from there — twice a frame, per list — and
//! those calls do enter the VM:
//!
//! ```text
//!                                             ┌───────────┐
//!                                             │ snapshot  │
//!                                             └───────────┘
//!                                                  │
//!                          many GPUI frames ◀──────┤
//!                                       materialize │
//!                                                  └──▶ VirtualList rows ──▶ VM
//! ```
//!
//! What it does not break is the claim the arrangement was built for. The VM is
//! entered for the *visible window* rather than for the collection, so a
//! ten-thousand-row list costs what a twenty-row one costs — and the
//! alternative, describing every row into the snapshot up front, is precisely
//! the cost virtualization exists to remove. Nothing else in an interface
//! repaints through script. [`crate::materialize`] states the exception in
//! full, along with the three things that confine it.

use std::rc::{Rc, Weak};

use crate::{
    engine::ShellRuntime,
    spec::{SpecArena, SpecId, StructureFingerprint},
};

/// One frozen description of a script view's interface.
///
/// Built by the engine and read by the native materializer; nothing mutates one
/// after it is published. A replacement is built beside it and swapped in whole,
/// so a script render that fails leaves the previous snapshot untouched.
#[derive(Clone)]
pub struct RenderSnapshot {
    inner: Rc<SnapshotInner>,
}

/// SMELT: 一次 `memo(key, deps, build)` 在这份描述里占的那一段。下一次构建同一个视图时，deps
/// 没变（JS 那边比）、主题没换，就把这一段拷过去，不再跑构建函数。
#[derive(Clone, Debug)]
pub(crate) struct MemoRecord {
    /// JS 那边记 deps 时配的号，两边对得上才算同一次记录。
    pub(crate) token: u64,
    pub(crate) start: SpecId,
    pub(crate) end: SpecId,
    pub(crate) root: SpecId,
    /// 这一段登记的处理函数，命中时原样转进新的一代。
    pub(crate) callbacks: Rc<[crate::spec::CallbackId]>,
    /// 这一段认领的虚拟列表行数。
    pub(crate) virtual_items: usize,
    /// 根在构建函数交出来那一刻的样子；之后调用方在外面往根上加的，拷的时候去掉。
    pub(crate) root_extent: crate::spec::RootExtent,
    /// 这一段里面（各层）记下的 `memo` 的 key：这一段被拷走时它们跟着搬。
    pub(crate) inner: Rc<[String]>,
}

/// SMELT: 一份描述里各个 `memo(...)` 的记录和录制时的主题版本。
#[derive(Default)]
pub(crate) struct MemoTable {
    pub(crate) records: std::collections::HashMap<String, MemoRecord>,
    pub(crate) theme_revision: u32,
}

struct SnapshotInner {
    /// Identifies the callbacks registered while this snapshot was built.
    generation: u64,
    /// SMELT: 这份描述里的 `memo(...)` 记录。
    memos: MemoTable,
    root: SpecId,
    arena: SpecArena,
    /// Weak so a snapshot never keeps the VM alive; a snapshot outliving its
    /// runtime has nothing to retire, and says so by failing to upgrade.
    runtime: Weak<ShellRuntime>,
    application: Option<Rc<crate::runtime::ApplicationGeneration>>,
    view: Option<gpui::WeakEntity<crate::ScriptView>>,
}

impl RenderSnapshot {
    pub(crate) fn new(
        runtime: &Rc<ShellRuntime>,
        generation: u64,
        root: SpecId,
        arena: SpecArena,
        application: Option<Rc<crate::runtime::ApplicationGeneration>>,
        view: Option<gpui::WeakEntity<crate::ScriptView>>,
    ) -> Self {
        Self::with_memos(
            runtime,
            generation,
            root,
            arena,
            application,
            view,
            MemoTable::default(),
        )
    }

    /// SMELT: 带上这次构建记下的 `memo(...)` 记录。
    pub(crate) fn with_memos(
        runtime: &Rc<ShellRuntime>,
        generation: u64,
        root: SpecId,
        arena: SpecArena,
        application: Option<Rc<crate::runtime::ApplicationGeneration>>,
        view: Option<gpui::WeakEntity<crate::ScriptView>>,
        memos: MemoTable,
    ) -> Self {
        Self {
            inner: Rc::new(SnapshotInner {
                memos,
                generation,
                root,
                arena,
                runtime: Rc::downgrade(runtime),
                application,
                view,
            }),
        }
    }

    pub(crate) fn application_owner(
        &self,
    ) -> Option<(
        Rc<crate::runtime::ApplicationGeneration>,
        gpui::WeakEntity<crate::ScriptView>,
    )> {
        Some((self.inner.application.clone()?, self.inner.view.clone()?))
    }

    pub(crate) fn root(&self) -> SpecId {
        self.inner.root
    }

    /// SMELT: 这份描述里的 `memo(...)` 记录。
    pub(crate) fn memos(&self) -> &MemoTable {
        &self.inner.memos
    }

    pub(crate) fn arena(&self) -> &SpecArena {
        &self.inner.arena
    }

    /// The levels this description spans, its root included.
    pub(crate) fn height(&self) -> u8 {
        self.inner.arena.height(self.inner.root)
    }

    /// The shape of this description, with its values left out.
    ///
    /// Compared against its predecessor's by [`crate::view::ScriptView`] to
    /// count how often a rebuild produced the structure it replaced. See
    /// [`StructureFingerprint`] for what that measurement is for and what it
    /// deliberately does not prove.
    pub(crate) fn structure(&self) -> StructureFingerprint {
        self.inner.arena.structure()
    }

    /// The description as text. Rendering never needs a GPU to be verified, and
    /// reading a published snapshot never needs the VM.
    pub fn debug_tree(&self) -> String {
        self.inner.arena.debug_tree(self.inner.root)
    }

    /// How many nodes the script described. Used by benchmarks to report cost
    /// per node rather than per view.
    pub fn len(&self) -> usize {
        self.inner.arena.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.arena.is_empty()
    }

    pub(crate) fn belongs_to(&self, runtime: &Rc<ShellRuntime>) -> bool {
        self.inner.runtime.ptr_eq(&Rc::downgrade(runtime))
    }
}

/// Retiring on drop is what keeps callback lifetime tied to snapshot lifetime
/// rather than to a frame or to a global render counter.
impl Drop for SnapshotInner {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.upgrade() {
            runtime.retire_callbacks(self.generation);
        }
    }
}
