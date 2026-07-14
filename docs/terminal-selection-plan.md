# 终端选区重构计划：跟随滚动 + 拖边缘自动滚动 + 跨屏复制

> 交接文档。这个功能在原会话里工具输出污染严重（Edit 回执假成功、grep/输出被截断、
> 还混入过一次 prompt injection），只做完了数据层就叫停，改到干净会话重做。仓库已恢复到
> 未改动状态（HEAD = v0.5.0）。**新会话照本文档从头实施即可，别沿用任何半成品。**

## 一、要解决的问题（用户报告）

终端里**无法一边滚动一边框选**：
1. 框选一段后往上滚，选区高亮停在原屏幕位置（那儿已经是别的内容），复制到的也是错内容。
2. 拖动到可视区上/下边缘不会自动滚动，所以选不到超过一屏的内容。

## 二、根因（已确诊）

跟之前的网格渲染重构无关，是**选区坐标系**加上**没有拖动自动滚动**两件事：

- **选区存的是「屏幕行号」而不是「缓冲区绝对行」**。
  `TerminalView.sel: Option<((usize,usize),(usize,usize))>`（`terminal_view.rs:128`）里的行号来自
  `pos_to_cell`（`terminal_view.rs:507`）的 `y / line_px`，是可视区 0..rows。而 `snapshot()`
  （`terminal.rs`）每帧按当前 `display_offset` 渲染可视区。两者一脱节：滚动改变 `display_offset`，
  可视区换成历史内容，但 `sel` 的屏幕行号没变 → 高亮漂移、`selected_text` 复制错内容。
- **`on_mouse_move`（`terminal_view.rs:928`）拖动只更新端点，没有「拖到边缘就自动滚动」**，
  所以拖不到屏幕外。

这两件事耦合，要「一边拖一边滚着选」必须一起修。

## 三、选定方案：交给 alacritty 原生 selection

**不要手搓绝对行坐标换算。** 用 `alacritty_terminal` 自带的 `Selection`，让它处理：滚动跟随、
跨屏复制（取当前不可见的 scrollback 行）、新输出到来时缓冲行漂移的维护、宽字符/语义/整行边界。
理由跟之前渲染照抄 Zed 一样——这些坐标细节坑多，且本机跑不出窗口没法交互验证，手搓风险高。

顺带收益：双击选词、三击选行换成 alacritty 的 `Semantic`/`Lines`，比现在手写的「向两侧扩到
空白」更准（正确处理路径、标点边界）。

## 四、alacritty_terminal 0.26 API 参考

> ⚠️ 下面签名来自调研（有幻觉风险，原会话两份报告有出入）。**实施时逐条对着 crate 源码
> 核实**：`~/.cargo/registry/src/index.crates.io-*/alacritty_terminal-0.26.0/src/` 下
> `index.rs` / `selection.rs` / `term/mod.rs` / `grid/mod.rs`。用 `cargo check` 的错误当裁判。

坐标与选区的关键事实（两份报告一致、且架构上成立的部分，可信度高）：

- **坐标类型**：`Point<Line, Column>`，`Line(i32)`、`Column(usize)`（`index.rs`）。`Side::{Left,Right}`
  表示端点落在 cell 左/右半。
- **屏幕行 → Point**：可视区第 r 行（0=顶）、列 c，用当前 `display_offset` 换算：
  `Point::new(Line(r as i32) - display_offset, Column(c))`。`term/mod.rs` 有现成的
  `viewport_to_point(display_offset, Point<usize>) -> Point` / `point_to_viewport(...)` 可用。
  **关键**：每次 `update` 都要用当前 `display_offset` 重算 —— 这就是滚动跟随的原理。
- **`Selection`**（`selection.rs`）：
  - `Selection::new(ty: SelectionType, location: Point, side: Side) -> Selection`
  - `SelectionType::{Simple, Block, Semantic, Lines}`（Semantic=选词，Lines=选行）
  - `update(&mut self, point: Point, side: Side)` —— 移动活动端
  - `to_range(&self, term) -> Option<SelectionRange>`
- **挂到 Term**：`term.selection: Option<Selection>` 是 **pub 字段**，直接 `term.selection = Some(sel)`
  / `= None` / `term.selection.as_mut().map(|s| s.update(p, side))`。
- **渲染判定**：`term.renderable_content()` 返回的 `RenderableContent` **已经带**
  `selection: Option<SelectionRange>` 字段（alacritty 内部就是 `term.selection.to_range(term)`）。
  `display_iter` 吐 `Indexed { point: Point, cell: &Cell }`。逐 cell：
  `sel_range.as_ref().is_some_and(|r| r.contains(indexed.point))`。
  `SelectionRange::contains(&self, Point) -> bool` 已正确处理 block vs 普通选区。
  **坐标同源**：`indexed.point` 和 `SelectionRange` 都是 `Point<Line>`，直接比，无需换算。
- **跨屏复制**：`term.selection_to_string() -> Option<String>`，内部遍历 grid 绝对行范围
  （含当前不可见的 scrollback），自己处理宽字符/wrap。**复制直接调它，别自己拼跨行文本。**

## 五、分步实施计划

### terminal.rs（数据层）

1. imports：
   ```rust
   use alacritty_terminal::index::{Column, Line, Point, Side};
   use alacritty_terminal::selection::{Selection, SelectionType};
   ```
2. `Cell` 加字段 `pub selected: bool`（`terminal.rs:66` 附近）。
3. `snapshot()`（`terminal.rs:742`）：
   - `let display_offset = content.display_offset;` 之后加 `let sel_range = content.selection;`
   - `for indexed in content.display_iter` 循环体开头加
     `let selected = sel_range.as_ref().is_some_and(|r| r.contains(indexed.point));`
   - `row.push(Cell { ... })` 里加 `selected,`
4. 新增对外类型（避免 terminal_view 直接依赖 alacritty 类型）：
   ```rust
   pub enum SelectionKind { Simple, Word, Line }
   ```
5. `impl Terminal` 新增方法（换算全封装在这里，terminal_view 只传可视区 (row,col)）：
   - `pub fn display_offset(&self) -> usize`
   - `pub fn selection_start(&mut self, row, col, kind: SelectionKind)` —— 构造 Point + `Selection::new`
     （kind → `Simple`/`Semantic`/`Lines`），`term.selection = Some(...)`
   - `pub fn selection_update(&mut self, row, col)` —— 用**当前** `display_offset` 重算 Point，
     `term.selection.as_mut().map(|s| s.update(p, Side::Right))`
   - `pub fn selection_clear(&mut self)` —— `term.selection = None`
   - `pub fn selection_text(&self) -> Option<String>` —— `term.selection_to_string()`
   - `pub fn has_selection(&self) -> bool` —— `selection_text().is_some_and(|s| !s.is_empty())`
     （给 mouse_up 判断「拖出了选区就别把点击转发给应用」用）

   注意 `Line(row as i32) - display_offset`（Line 有 `Sub<usize>` impl，若报错改
   `Line(row as i32 - display_offset as i32)`），列 clamp 到 `Column(0..cols)`。

### terminal_view.rs（交互 + 渲染）

6. 字段（`terminal_view.rs:128`）：删 `sel`，改为
   ```rust
   selecting: bool,          // 是否正在拖动框选
   drag_scroll: i32,         // 拖到边缘的自动滚动方向：0 不滚，+向上看历史，-向下
   drag_scroll_col: usize,   // 自动滚动时选区活动端用的列
   drag_scroll_running: bool // 防重复 spawn 定时器
   ```
   对应改 `Self::new`（`:357`）初值、reattach 重置处（`:422` 的 `self.sel = None` → `selection_clear()`）。
7. 删除 `selected_text`（`:517`）、`word_at`（`:568`）、`line_at`（`:586`）。
   保留 `char_steps_between`（`:558`，Option+点击移动光标用，不涉及选区）。
8. `render_row`（`:1052`）：去掉 `sel` 参数，`style_of` 里 `is_sel(i)` → `row[i].selected`。
   删除 `sel_range_for_row`（`:1190` 定义、`:1001` 调用），调用点改
   `render_row(row, cc, &base_font, hl, cell_w)`；删 `:762` 的 `let sel = self.sel;`。
9. `mouse_down`（`:920`）：
   ```rust
   let kind = match ev.click_count { 2 => Word, n if n>=3 => Line, _ => Simple };
   this.terminal.selection_start(cell.0, cell.1, kind);
   this.selecting = true;
   ```
   （Cmd+点击开链接、Option+点击移光标的分支保留，它们 return 前不进选区。）
10. `mouse_move`（`:928`）：`ev.pressed_button == Left && this.selecting` 时
    `selection_update(row,col)`；并检测鼠标 y 是否出可视区上/下界（用 `grid_origin`+`grid_size`），
    出界则设 `drag_scroll` 方向并启动自动滚动定时器（见下）。
11. `mouse_up`（`:960`）：`this.selecting = false; this.drag_scroll = 0;`，原
    `this.sel.is_some_and(|(a,b)| a!=b)` 改 `this.terminal.has_selection()`。
12. Cmd+C 复制（`:824`）：`this.selected_text()` → `this.terminal.selection_text()`。
13. **自动滚动定时器**（新方法，`smol::Timer` 已 import）：
    ```rust
    fn start_drag_scroll(&mut self, cx) {
        if self.drag_scroll_running { return; }
        self.drag_scroll_running = true;
        cx.spawn(async move |this, cx| loop {
            Timer::after(Duration::from_millis(60)).await;
            let go = this.update(cx, |this, cx| {
                if !this.selecting || this.drag_scroll == 0 { this.drag_scroll_running = false; return false; }
                let dir = this.drag_scroll;
                this.terminal.scroll(dir);          // 已有方法，正=向上看历史
                let row = if dir > 0 { 0 } else { /*可视区最后一行*/ };
                this.terminal.selection_update(row, this.drag_scroll_col);
                cx.notify();
                true
            });
            if !matches!(go, Ok(true)) { break; }
        }).detach();
    }
    ```
    方向：往上拖(y<0)想看更早内容 → `scroll(+1)`；往下拖(y>高) → `scroll(-1)`。
    可视区行数用 `grid_size` 高度 / `line_px()`。

### 测试

14. `terminal_view.rs` 测试里 `row()` 辅助（`:1462` 附近）构造 `Cell` 要补 `selected: false`。
15. 逻辑纯函数尽量抽出来单测（渲染/交互本机没法验，最后靠用户拖拽验收）。

## 六、验收（本机跑不出窗口，必须用户在真机拖拽验证）

- 框选一段 → 滚动 → 高亮跟着内容走、复制内容正确
- 拖到上/下边缘 → 持续自动滚动并扩选，能选超过一屏
- 双击选词 / 三击选行边界正确
- 选区期间有新输出（shell 打印）时高亮不错乱
- 顺带回归：光标反色、Cmd+点击链接、Option+点击移光标、选区背景高亮无缝隙

## 七、本会话已验证的中间结论（可直接采信）

- 数据层四处改动（Cell.selected、snapshot 的 sel_range + per-cell contains + push）改完后
  `cargo check --bin workspace` **0 错误**，已验证可行。已回滚，按上表重做即可。
- `contains(indexed.point)` 的坐标同源判定成立（不用手动换算 Line↔usize）。
