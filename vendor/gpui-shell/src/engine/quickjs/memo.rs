//! SMELT: `memo(key, deps, build)` —— 一个视图重建时，没变的那几块不再进 JS。
//!
//! 快照（§8.4）省掉的是「视图没变」：没被通知就不跑脚本。视图一被通知，整段 `render` 照样从头
//! 描述一遍，列表里只有一行变了也是几百个节点、几千次过桥。`memo` 把这一段切小：
//!
//! ```text
//! 上一次构建   memo("row:a", [row], build)  ──▶  录下这一段：[start, end)、根、处理函数
//! 这一次构建   deps 一样（JS 比）、主题没换  ──▶  把那一段拷过来，build 不跑
//!              deps 变了                      ──▶  跑 build，录下新的一段
//! ```
//!
//! 和模板（`template.rs`）是两回事：模板沿用结构、换值，要按它的规矩写；`memo` 沿用值没变的
//! 整段描述，构建函数就是普通代码，规矩只有 React 的那一条——构建函数读到的东西都要在 deps 里。
//!
//! # 谁记什么
//!
//! - **deps 在 JS 那边**：每个视图对象一张表（prelude 的 `memoTables`），随视图对象回收。
//!   Rust 这边不存 JS 值：快照可能比虚拟机活得长，`Persistent` 晚于运行时释放会让进程中止。
//! - **段在快照里**（[`crate::snapshot::MemoTable`]）：每份描述记下自己里面的各段。下一次构建
//!   从视图当前那份描述里拷。两边用同一个号（token）对上：JS 记 deps 时配一个，Rust 记段时存
//!   同一个，对不上就当没有。
//! - **处理函数跟着段走**：拷过来的段里还是原来的回调 id，命中时把这些 id 原样再登记进新的一代
//!   （[`crate::runtime::CallbackArena::restage`]），旧描述收走时不会把它们一起收掉。
//!
//! # 嵌套
//!
//! 外层命中时整段拷过来，里面的 `memo` 不会再被调用；它们的记录得跟着搬进新描述，不然外层下
//! 次没命中时里面全要重做。所以每条记录带着它里面（各层）的 key，命中时一起搬。
//!
//! # 不录的情况
//!
//! 构建函数用了外面建的元素、或把里面的元素交到外面（[`crate::spec::SpecArena::self_contained`]），
//! 拷过去就不对了：这一段照常画，只是不记，下次还跑构建函数。虚拟列表的行在布局时描述，不在
//! 一次构建里，`memo` 在那里直接调构建函数。

use std::collections::HashSet;

use rquickjs::{Ctx, Result as JsResult};

use super::{ShellRuntime, js_error};
use crate::{
    snapshot::{MemoRecord, MemoTable, RenderSnapshot},
    spec::SpecId,
};

/// 一次构建里 `memo` 的状态：从哪份描述拷、这次记下了什么、哪些 key 用过了、正在录的几层。
pub(super) struct MemoBuild {
    source: Option<RenderSnapshot>,
    table: MemoTable,
    used: HashSet<String>,
    stack: Vec<Frame>,
}

/// 一层正在录的 `memo`。
struct Frame {
    key: String,
    token: u64,
    start: SpecId,
    /// 打开时回调登记到哪了，录完取这之后登记的。
    callbacks: usize,
    virtual_items: usize,
    /// 里面各层已经录下（或搬过来）的 key。
    inner: Vec<String>,
}

/// `__memo_enter` 的回答：拷过来了（根的 id）、要跑构建函数并录下、或这里不能 memo 只管跑。
const BUILD: i64 = -1;
const BYPASS: i64 = -2;

impl MemoBuild {
    /// `source` 是视图当前那份描述。主题换了，那份描述里解析好的颜色都不作数，一段也不拷。
    pub(super) fn new(source: Option<RenderSnapshot>, theme_revision: u32) -> Self {
        let source = source.filter(|source| {
            source.memos().theme_revision == theme_revision && !source.memos().records.is_empty()
        });
        Self {
            source,
            table: MemoTable {
                records: Default::default(),
                theme_revision,
            },
            used: HashSet::new(),
            stack: Vec::new(),
        }
    }

    pub(super) fn into_table(self) -> MemoTable {
        self.table
    }

    /// 一次构建里一个 key 只能用一次：同一段拷两份，嵌套视图会挂两处，处理函数也分不清。
    fn claim<'a>(
        &mut self,
        ctx: &Ctx<'_>,
        keys: impl IntoIterator<Item = &'a String>,
    ) -> JsResult<()> {
        for key in keys {
            if !self.used.insert(key.clone()) {
                return Err(js_error::type_error(
                    ctx,
                    &format!(
                        "memo key `{key}` is used twice in one render. Each memo needs its own \
                         key, the way list rows do"
                    ),
                ));
            }
        }
        Ok(())
    }
}

impl ShellRuntime {
    /// 开始一次构建的 `memo` 记账。
    pub(super) fn open_memo_build(&self, source: Option<RenderSnapshot>, theme_revision: u32) {
        *self.memo.borrow_mut() = Some(MemoBuild::new(source, theme_revision));
    }

    /// 结束记账，交出这次构建记下的各段。构建失败时也调，丢掉就是。
    pub(super) fn close_memo_build(&self) -> MemoTable {
        self.memo
            .borrow_mut()
            .take()
            .map(MemoBuild::into_table)
            .unwrap_or_default()
    }

    /// 诊断重跑同一次构建前，清掉第一次记下的。
    pub(super) fn restart_memo_build(&self) {
        let mut memo = self.memo.borrow_mut();
        if let Some(build) = memo.as_mut() {
            let source = build.source.take();
            *build = MemoBuild::new(source, build.table.theme_revision);
        }
    }

    /// `memo(key, …)` 进来：`token` 是 JS 那边 deps 没变时上次配的号（变了是 -1），`next` 是这次
    /// 配的号。答根的 id（拷过来了）、[`BUILD`]（跑构建函数，录完调 `__memo_exit`）或 [`BYPASS`]。
    pub(super) fn enter_memo(
        &self,
        ctx: &Ctx<'_>,
        key: String,
        token: f64,
        next: f64,
    ) -> JsResult<i64> {
        // 模板在发现阶段换了一块空的描述，位置对不上。
        if self.discovery.borrow().is_some() {
            return Ok(BYPASS);
        }
        let mut memo = self.memo.borrow_mut();
        let Some(build) = memo.as_mut() else {
            return Ok(BYPASS);
        };
        let next = next as u64;
        if token >= 0.0
            && let Some(root) = self.reuse_memo(ctx, build, &key, token as u64, next)?
        {
            return Ok(i64::from(root));
        }
        build.claim(ctx, [&key])?;
        let arena = self.arena.borrow();
        build.stack.push(Frame {
            key,
            token: next,
            start: arena.len() as SpecId,
            callbacks: self.callbacks.borrow().checkpoint(),
            virtual_items: arena.virtual_items(),
            inner: Vec::new(),
        });
        Ok(BUILD)
    }

    /// 上一份描述里有这一段、号对得上、处理函数都还在，就拷过来。
    fn reuse_memo(
        &self,
        ctx: &Ctx<'_>,
        build: &mut MemoBuild,
        key: &String,
        token: u64,
        next: u64,
    ) -> JsResult<Option<SpecId>> {
        let Some(source) = build.source.clone() else {
            return Ok(None);
        };
        let records = &source.memos().records;
        let Some(record) = records.get(key).filter(|record| record.token == token) else {
            return Ok(None);
        };
        if !self.callbacks.borrow().all_live(&record.callbacks) {
            return Ok(None);
        }
        build.claim(ctx, std::iter::once(key).chain(record.inner.iter()))?;

        let mut arena = self.arena.borrow_mut();
        let base = arena.len() as SpecId;
        let root = arena
            .graft_range(
                source.arena(),
                record.start,
                record.end,
                record.root,
                Some(record.root_extent),
                crate::spec::memo_shape(key),
            )
            .map_err(|error| js_error::type_error(ctx, &error.to_string()))?;
        arena.add_virtual_items(record.virtual_items);
        drop(arena);
        self.callbacks.borrow_mut().restage(&record.callbacks);

        // 这一段和里面各层一起搬进新描述，位置跟着平移；里面各层的号不变，JS 那边也原样搬。
        let moved = |position: SpecId| position - record.start + base;
        let carry = |inner: &MemoRecord, token: u64| MemoRecord {
            token,
            start: moved(inner.start),
            end: moved(inner.end),
            root: moved(inner.root),
            root_extent: inner.root_extent,
            callbacks: inner.callbacks.clone(),
            virtual_items: inner.virtual_items,
            inner: inner.inner.clone(),
        };
        for inner_key in record.inner.iter() {
            if let Some(inner) = records.get(inner_key) {
                build
                    .table
                    .records
                    .insert(inner_key.clone(), carry(inner, inner.token));
            }
        }
        build.table.records.insert(key.clone(), carry(record, next));
        if let Some(parent) = build.stack.last_mut() {
            parent.inner.push(key.clone());
            parent.inner.extend(record.inner.iter().cloned());
        }
        self.metrics
            .record_memo_hit((record.end - record.start) as usize);
        Ok(Some(root))
    }

    /// 构建函数跑完：这一段自成一体就记下。答记没记，JS 那边照样记 deps。
    pub(super) fn exit_memo(&self, ctx: &Ctx<'_>, root: Option<SpecId>) -> JsResult<bool> {
        let mut memo = self.memo.borrow_mut();
        let Some(frame) = memo.as_mut().and_then(|build| build.stack.pop()) else {
            return Err(js_error::type_error(ctx, "no memo is being built"));
        };
        self.metrics.record_memo_miss();
        let arena = self.arena.borrow();
        let Some(root) = root.filter(|root| arena.self_contained(frame.start, *root)) else {
            return Ok(false);
        };
        let build = memo.as_mut().expect("the frame came from it");
        let record = MemoRecord {
            token: frame.token,
            start: frame.start,
            end: arena.len() as SpecId,
            root,
            root_extent: arena.root_extent(root),
            callbacks: self.callbacks.borrow().staged_since(frame.callbacks).into(),
            virtual_items: arena.virtual_items().saturating_sub(frame.virtual_items),
            inner: frame.inner.into(),
        };
        if let Some(parent) = build.stack.last_mut() {
            parent.inner.push(frame.key.clone());
            parent.inner.extend(record.inner.iter().cloned());
        }
        build.table.records.insert(frame.key, record);
        Ok(true)
    }

    /// 构建函数抛了：丢掉这一层，异常照常往外走。
    pub(super) fn abort_memo(&self) {
        if let Some(build) = self.memo.borrow_mut().as_mut() {
            build.stack.pop();
        }
    }
}
