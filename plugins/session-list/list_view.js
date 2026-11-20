// 会话列表视图：滚动区里的各行（rows.js）、行内改名、「项目」标题上的搜索、确认框、可视区外的
// 胶囊、动画账。状态都在 SessionList（state.js）上，各视图共用；这里只留这一份视图自己的：改名、
// 搜索的输入框，正在问的确认，滚动区报来的可视区外的行，动画账。
//
// render 只读内存。侧栏换了一份、选中换了，在 render 开头比一下前后，记动画账、要滚到哪一行
// （只改这份视图的内存，不做 IO、不叫重画）。

import { div, View } from "gpui-kit";
import { opened_view, notify_view, slot_views, t, theme } from "smelt-plugin-api";
import { SmeltButton, SmeltDialog, SmeltInputState, SmeltScrollView, SmeltWindowBar } from "smelt-ui";

import * as actions from "./actions.js";
import { LIST, SESSION_AVATAR } from "./contract.js";
import { menus } from "./menus.js";
import { pick_avatar, same_conversation, selected_session } from "./model.js";
import { ListMotion } from "./motion.js";
import { new_session_button } from "./new_session_view.js";
import { hint, tally_is_empty, tally_label } from "./offscreen.js";
import * as rows from "./rows.js";
import { session_flip } from "./sidebar.js";

/** 还开着的列表视图（列表是单例，布局里可能一时有两份）。FOCUS_LIST 把焦点交给它们。 */
const open_views = new Set();

/** 列表被打开（或又打开一次）：参数 `{ focus: true }` 是把键盘焦点交给列表。 */
export function open_list(instance, args) {
  for (const view of open_views) {
    if (view.instance === instance && args?.focus) view.focus.focus();
  }
}

/** list 是插件共用的 SessionList。 */
export function list_view_class(list) {
  return class ListView extends View {
    init(_props, cx) {
      const opened = opened_view();
      this.instance = opened.instance;
      this.focus = cx.focus_handle();
      /** 正在改名：{ target, original, state }。target 是 { kind: "conversation", key } | { kind: "workset", machine, id }。 */
      this.rename = null;
      /** 正在搜：{ state, pushed }（pushed 是推给 SessionList 的那份字，别处清掉了就收起）。 */
      this.search = null;
      /** 正在问的确认：{ title, action, description, danger, confirm }。 */
      this.asking = null;
      /** 滚动区报来的、在可视区上面和下面的要你看的行。 */
      this.above = [];
      this.below = [];
      this.motion = new ListMotion(list.sidebar);
      this.seen_sidebar = list.sidebar;
      this.selected = selected_session(list.selection);
      /** 要滚到的那一行：key 换了或 serial 变了就滚一次。 */
      this.reveal_key = null;
      this.reveal_serial = 0;
      /** 正开着的「新建会话」弹层：哪个按钮的、菜单状态、焦点（new_session_view.js）。 */
      this.new_session = null;
      this.ask = (question) => {
        this.asking = question;
        this.redraw();
      };
      open_views.add(this);
      // 列表是被 REVEAL_IN_LIST 打开才建的：要找的会话这时才取得到。
      this.take_located();
      if (/** @type {{ focus?: boolean } | null} */ (opened.args)?.focus) this.focus.focus();
    }

    redraw() {
      if (notify_view(this.instance)) return;
      // 视图没了：开着的新建菜单也不再收事件。
      open_views.delete(this);
      this.new_session?.menu.dispose();
      this.new_session = null;
    }

    // ---- 改名 ----

    /** 开始行内改名：输入框换下标题，全选原名。回车、点别处保存，Esc 取消。 */
    start_rename(target, original) {
      list.release_retired_inputs();
      if (this.rename) list.retire_input(this.rename.state);
      const state = SmeltInputState.new();
      state.set_value(original);
      state.on("submit", () => {
        this.commit_rename();
        this.focus.focus();
      });
      state.on("blur", () => this.commit_rename());
      this.rename = { target, original, state };
      state.focus();
      state.select_all();
      this.redraw();
    }

    /** 存下改名。先取走状态：回车之后输入框还会失焦，第二次进来时已经没有了，不会存两遍。 */
    commit_rename() {
      const rename = this.rename;
      if (!rename) return;
      this.rename = null;
      list.retire_input(rename.state);
      this.redraw();
      list.commit_rename(rename.target, rename.original, rename.state.value());
    }

    cancel_rename() {
      const rename = this.rename;
      if (!rename) return;
      this.rename = null;
      list.retire_input(rename.state);
      this.focus.focus();
      this.redraw();
    }

    // ---- 搜索 ----

    /**
     * 点「项目」标题：标题换成搜索框并拿到焦点，边打边按名字搜项目。已经在搜就只把焦点放回去。
     * Esc、清空后点别处收起。
     */
    start_search() {
      if (this.search) {
        this.search.state.focus();
        return;
      }
      list.release_retired_inputs();
      const state = SmeltInputState.new(t("list.search_projects"));
      const search = { state, pushed: "" };
      state.on("change", () => {
        search.pushed = state.value();
        list.set_project_filter(search.pushed);
      });
      // 回车：焦点交回列表，搜索留着，好用方向键在搜到的会话里走。
      state.on("submit", () => this.focus.focus());
      // 没字了还点别处就收起（焦点留在点到的地方）；有字留着，标题一直是搜索框，看得出列表是滤过的。
      state.on("blur", () => {
        if (state.value() === "") this.close_search();
      });
      this.search = search;
      state.focus();
      this.redraw();
    }

    /** 收起搜索框，回到「项目」标题，列表回到不搜。返回原来有没有在搜。 */
    close_search() {
      const search = this.search;
      if (!search) return false;
      this.search = null;
      list.retire_input(search.state);
      list.set_project_filter("");
      this.redraw();
      return true;
    }

    /** Esc：在改名就取消改名，在搜就收起搜索，焦点交回列表。 */
    escape() {
      if (this.rename) this.cancel_rename();
      else if (this.close_search()) this.focus.focus();
    }

    // ---- 侧栏变了 ----

    /**
     * 侧栏换了一份或选中换了：选中的会话滚进来；重排、刚开始跑、刚在这里新建的会话露面了，
     * 记进动画账。只改这份视图和 SessionList 上交给视图取走的那两项。
     */
    sidebar_changed() {
      const sidebar = list.sidebar;
      // 搜索在别处被清掉了（从通知跳到滤掉的项目里的会话，见 SessionList.reveal）：框也收起。
      if (this.search && this.search.pushed !== list.project_filter) {
        list.retire_input(this.search.state);
        this.search = null;
      }
      const selected = selected_session(list.selection);
      if (!same_conversation(selected, this.selected)) {
        this.selected = selected;
        if (selected) this.scroll_to(session_flip(selected));
      }
      if (sidebar === this.seen_sidebar) return;
      this.seen_sidebar = sidebar;
      const now = Date.now();
      this.motion.track(sidebar, now);
      if (list.fresh && sidebar.has_session(list.fresh)) {
        this.motion.arrive(session_flip(list.fresh), now);
        list.fresh = null;
      }
      this.take_located();
    }

    /** 有要找出来的会话（REVEAL_IN_LIST）就取走：滚过去、闪一下。 */
    take_located() {
      const key = list.located;
      if (!key) return;
      list.located = null;
      const flip_key = session_flip(key);
      this.scroll_to(flip_key);
      this.motion.locate(flip_key, Date.now());
    }

    scroll_to(key) {
      this.reveal_key = key;
      this.reveal_serial += 1;
    }

    // ---- 画 ----

    render() {
      const selected = selected_session(list.selection);
      if (list.sidebar !== this.seen_sidebar || !same_conversation(selected, this.selected)) this.sidebar_changed();
      const sidebar = list.sidebar;
      const look = theme();
      const notices = sidebar.notices_besides(selected);
      const offscreen = hint(notices, this.above, this.below);
      const view = this;
      const ctx = {
        list,
        view,
        t,
        theme: look,
        sidebar,
        selection: list.selection,
        now: Math.floor(Date.now() / 1000),
        motion: this.motion,
        avatar_view: pick_avatar(
          list.avatar,
          slot_views(SESSION_AVATAR).map((info) => info.id),
        ),
        rename: this.rename,
        search: this.search?.state ?? null,
        menus: null,
        new_session_button: (id, tooltip, machine, path, hover_group) =>
          new_session_button(list, view, id, tooltip, machine, path, hover_group),
      };
      ctx.menus = menus(ctx);
      let scroll = SmeltScrollView.new("sessions-list-scroll")
        .flex_1()
        .min_h_0()
        .edges()
        .watch(notices.map((notice) => notice.key))
        .on_offscreen((above, below) => {
          this.above = above;
          this.below = below;
          this.redraw();
        })
        .on_drop_files((paths) => actions.open_dropped_paths(list, paths))
        .child(div().flex().flex_col().pb_3().children(rows.sidebar(ctx)));
      if (this.reveal_key != null) scroll = scroll.reveal(this.reveal_key, this.reveal_serial);
      const above = pill(list, offscreen.above, true);
      const below = pill(list, offscreen.below, false);
      if (above) scroll = scroll.overlay_top(above);
      if (below) scroll = scroll.overlay_bottom(below);
      return div()
        .id("sessions-list")
        .key_context("SessionsList")
        .track_focus(this.focus)
        .size_full()
        .flex()
        .flex_col()
        .child(SmeltWindowBar.new(LIST))
        .child(scroll)
        .when(this.asking != null, (root) => root.child(confirm_dialog(this)));
    }
  };
}

/**
 * 滚到可视区外、要你看一眼的会话：那一边浮一个小胶囊，点一下打开最近的那一段。出口里可能正放着
 * 别的页（归档这些）：和点那一行一样，打开会话视图再选中。
 */
function pill(list, side, up) {
  const session = side.nearest;
  if (session == null || tally_is_empty(side.tally)) return null;
  const button = SmeltButton.new(up ? "sessions-offscreen-above" : "sessions-offscreen-below")
    .floating()
    .small()
    .icon(up ? "icons/arrow-up.svg" : "icons/arrow-down.svg")
    .label(tally_label(side.tally, t))
    .on_click(() =>
      list.report_row_failure(
        { kind: "conversation", key: session },
        t("action.open_session"),
        list.open_conversation(session),
      ),
    );
  return div()
    .flex()
    .justify_center()
    .when(up, (row) => row.pt_2())
    .when(!up, (row) => row.pb_2())
    .child(button);
}

/**
 * 确认框：取消在左，动作在右（危险的用危险实底）。危险的确认 Enter 只关框不执行，要点按钮。
 * 点背景不关。
 */
function confirm_dialog(view) {
  const asking = view.asking;
  const close = () => {
    if (view.asking !== asking) return;
    view.asking = null;
    view.redraw();
  };
  const run = () => {
    close();
    asking.confirm();
  };
  const action = SmeltButton.new("dialog-confirm").label(asking.action).on_click(run);
  return SmeltDialog.new("sessions-confirm")
    .open(true)
    .w(440)
    .title(asking.title)
    .when(asking.description != null, (dialog) => dialog.description(asking.description))
    .close_on_backdrop(false)
    .footer(
      div()
        .w_full()
        .flex()
        .justify_end()
        .gap_2()
        .child(SmeltButton.new("dialog-cancel").quiet().label(t("action.cancel")).on_click(close))
        .child(asking.danger ? action.danger() : action.primary()),
    )
    .on_confirm(() => (asking.danger ? close() : run()))
    .on_dismiss(close);
}
