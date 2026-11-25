// 对话流压测探针：200 条变高消息（用户气泡、Markdown + 代码高亮 + 表格 + Mermaid 多彩 SVG + 工具卡），
// 配合顶部可见项追踪（PromptRail 阅读线）、跨气泡选区、以及底栏多行富输入（IME / 粘贴图 / 光标坐标）。
// `intervalMs > 0` 时底部最新助手消息按对应频率持续流式追加 Markdown token，用于验证 60fps / 30fps 帧耗时与粘底。

import { cleanup, Image, List, signal, Text, View } from "nsh";
import { Input, TextView } from "nsh/base";

// 真实多色 SVG 流程图（预编码 base64，走 NSH GPUI 全彩 SVG 光栅化管线，验证非单色 mask 渲染）。
// <svg xmlns="http://www.w3.org/2000/svg" width="420" height="96" viewBox="0 0 420 96">
//   <rect x="8" y="20" width="110" height="56" rx="8" fill="#1e293b" stroke="#38bdf8" stroke-width="2"/>
//   <text x="63" y="53" text-anchor="middle" fill="#f8fafc" font-size="13">ACP Stream</text>
//   <path d="M118 48 L156 48" stroke="#94a3b8" stroke-width="2"/>
//   <rect x="156" y="20" width="110" height="56" rx="8" fill="#1e293b" stroke="#a78bfa" stroke-width="2"/>
//   <text x="211" y="53" text-anchor="middle" fill="#f8fafc" font-size="13">NSH List</text>
//   <path d="M266 48 L304 48" stroke="#94a3b8" stroke-width="2"/>
//   <rect x="304" y="20" width="108" height="56" rx="8" fill="#1e293b" stroke="#34d399" stroke-width="2"/>
//   <text x="358" y="53" text-anchor="middle" fill="#f8fafc" font-size="13">GPUI Paint</text>
// </svg>
const MERMAID_SVG_URI =
  "data:image/svg+xml;base64," +
  "PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHdpZHRoPSI0MjAiIGhlaWdodD0iOTYiIHZpZXdCb3g9IjAgMCA0MjAgOTYiPgogIDxyZWN0IHg9IjgiIHk9IjIwIiB3aWR0aD0iMTEwIiBoZWlnaHQ9IjU2IiByeD0iOCIgZmlsbD0iIzFlMjkzYiIgc3Ryb2tlPSIjMzhiZGY4IiBzdHJva2Utd2lkdGg9IjIiLz4KICA8dGV4dCB4PSI2MyIgeT0iNTMiIHRleHQtYW5jaG9yPSJtaWRkbGUiIGZpbGw9IiNmOGZhZmMiIGZvbnQtc2l6ZT0iMTMiPkFDUCBTdHJlYW08L3RleHQ+CiAgPHBhdGggZD0iTTExOCA0OCBMMTU2IDQ4IiBzdHJva2U9IiM5NGEzYjgiIHN0cm9rZS13aWR0aD0iMiIvPgogIDxyZWN0IHg9IjE1NiIgeT0iMjAiIHdpZHRoPSIxMTAiIGhlaWdodD0iNTYiIHJ4PSI4IiBmaWxsPSIjMWUyOTNiIiBzdHJva2U9IiNhNzhiZmEiIHN0cm9rZS13aWR0aD0iMiIvPgogIDx0ZXh0IHg9IjIxMSIgeT0iNTMiIHRleHQtYW5jaG9yPSJtaWRkbGUiIGZpbGw9IiNmOGZhZmMiIGZvbnQtc2l6ZT0iMTMiPk5TSCBsaXN0PC90ZXh0PgogIDxwYXRoIGQ9Ik0yNjYgNDggTDMwNCA0OCIgc3Ryb2tlPSIjOTRhM2I4IiBzdHJva2Utd2lkdGg9IjIiLz4KICA8cmVjdCB4PSIzMDQiIHk9IjIwIiB3aWR0aD0iMTA4IiBoZWlnaHQ9IjU2IiByeD0iOCIgZmlsbD0iIzFlMjkzYiIgc3Ryb2tlPSIjMzRkMzk5IiBzdHJva2Utd2lkdGg9IjIiLz4KICA8dGV4dCB4PSIzNTgiIHk9IjUzIiB0ZXh0LWFuY2hvcj0ibWlkZGxlIiBmaWxsPSIjZjhmYWZjIiBmb250LXNpemU9IjEzIj5HUFVJIFBhaW50PC90ZXh0Pgo8L3N2Zz4=";

const MERMAID_EXTENSIONS = [{ name: "mermaid", language: "mermaid" }];

const STREAM_BODY = `## 方案分析与流式验证

正在把 **conversation** 的消息流切换到 NSH 原生虚拟列表 \`<List>\` + \`<TextView options={{ streaming: true }}>\`：

1. **虚拟化与回收**：200 条历史消息按视口回收，不把离屏节点挂进布局树。
2. **局部信号更新**：只有当前正在吐字的最后一条消息绑定信号，历史气泡零重绘。
3. **语法高亮与 Mermaid**：代码块走 \`syntect\`，Mermaid 走扩展节点全彩 SVG。

\`\`\`rust
pub fn apply_delta(mirror: &mut FrameMirror, delta: FrameDelta) -> usize {
    let applied = mirror.apply(delta);
    tracing::debug!(count = applied, "applied streaming acp delta");
    applied
}
\`\`\`

| 维度 | 目标值 | 说明 |
| --- | ---: | --- |
| 主线程帧耗时 | <= 4 ms | 60Hz 连续吐字不掉帧 |
| 空闲唤醒 | 0 次/秒 | 停住后主线程完全静默 |
| 粘底稳态 | 0 跳动 | maintainVisibleContentPosition 自动锚定底部 |

\`\`\`mermaid
flowchart LR
  ACP --> NSH --> GPUI
\`\`\`

以上是完整的单轮流式回答，用来压测行高持续增长时的布局与重绘成本。`;

const STREAM_TOKENS = STREAM_BODY.match(/\s*\S{1,4}|\s+/g) ?? [STREAM_BODY];

interface ChatItem {
  id: number;
  kind: "streaming" | "user" | "assistant" | "tool";
  text: string;
  toolTitle?: string;
}

function buildMessages(): ChatItem[] {
  const items: ChatItem[] = [];
  for (let i = 0; i < 199; i++) {
    const mod = i % 4;
    if (mod === 0) {
      items.push({
        id: i,
        kind: "user",
        text: `第 ${Math.floor(i / 4) + 1} 问：检查 crates/smelt-nsh 里变高虚拟列表在流式追加 token 时主线程耗时与粘底是否稳定？`,
      });
    } else if (mod === 1) {
      items.push({
        id: i,
        kind: "tool",
        toolTitle: `Read crates/smelt-nsh/src/plugins/sdk.tsx (${80 + (i % 40)} lines)`,
        text: "Completed in 18 ms · +24 -6",
      });
    } else if (mod === 2) {
      const withMermaid = i === 194 || i === 182;
      items.push({
        id: i,
        kind: "assistant",
        text:
          `已完成排查（条目 #${i}）。关键路径如下：\n\n` +
          "```rust\n" +
          `fn row_${i}(delta: usize) -> usize {\n    delta.saturating_add(${i})\n}\n` +
          "```\n\n" +
          (withMermaid
            ? "```mermaid\nflowchart LR\n  A[Stream] --> B[List]\n```\n"
            : "- 虚拟列表只测可见区\n- 历史条目不订阅流式信号"),
      });
    } else {
      items.push({
        id: i,
        kind: "assistant",
        text: `简短确认 #${i}：已核对 \`nsh-platform-gpui\` 的多行 \`Input\`、\`onCaretMove\` 与 \`onPaste\` 接线，均已在桌面端实现。`,
      });
    }
  }
  // 按自然时间顺序排：最后一条是底部最新助手消息
  items.push({ id: 199, kind: "streaming", text: STREAM_BODY });
  return items;
}

const MESSAGES = buildMessages();

function renderMermaidCustom(node: { name: string; text?: string }) {
  if (node.name !== "mermaid") return null;
  return (
    <View
      style={{
        padding: 8,
        borderRadius: 8,
        backgroundColor: "#0f172a",
        borderWidth: 1,
        borderColor: "#334155",
      }}
    >
      <Image
        src={MERMAID_SVG_URI}
        style={{ width: 420, height: 96, objectFit: "contain" }}
      />
    </View>
  );
}

const MARKDOWN_STYLES = {
  root: { color: "#e5e7eb", fontSize: 14, lineHeight: 22 },
  code: { backgroundColor: "#1f2937", borderRadius: 4 },
  codeBlock: { backgroundColor: "#111827", borderRadius: 6, borderColor: "#374151" },
};

export function ChatStream(props: { intervalMs: number }) {
  const streaming = signal(props.intervalMs > 0);
  const streamText = signal(props.intervalMs > 0 ? "" : STREAM_BODY);
  const visibleRange = signal("—");
  const draft = signal("");
  const caretInfo = signal("");
  const pasteInfo = signal("");
  const inputHeight = signal(44);

  if (props.intervalMs > 0) {
    let idx = 0;
    const timer = setInterval(() => {
      if (idx >= STREAM_TOKENS.length) {
        idx = 0;
        streamText.set("");
        return;
      }
      const chunk = STREAM_TOKENS.slice(idx, idx + 3).join("");
      idx += 3;
      streamText.set(streamText() + chunk);
    }, props.intervalMs);
    cleanup(() => clearInterval(timer));
  }

  return (
    <View style={{ flex: 1, backgroundColor: "#09090b" }}>
      {/* 顶部状态条：验证 PromptRail 所需的 onViewableItemsChange 回调 */}
      <View
        style={{
          height: 32,
          paddingHorizontal: 16,
          flexDirection: "row",
          alignItems: "center",
          justifyContent: "space-between",
          borderBottomWidth: 1,
          borderBottomColor: "#27272a",
        }}
      >
        <Text style={{ fontSize: 12, color: "#a1a1aa" }}>
          {props.intervalMs > 0
            ? `ChatStream · 200 items · ${Math.round(1000 / props.intervalMs)}Hz streaming`
            : "ChatStream · 200 items · idle"}
        </Text>
        <Text style={{ fontSize: 12, color: "#71717a" }}>
          visible: {visibleRange()} {caretInfo()} {pasteInfo()}
        </Text>
      </View>

      {/* 自然时间序虚拟消息流：initialScrollAtEnd + maintainVisibleContentPosition 自动粘底 */}
      <List
        each={MESSAGES}
        initialScrollAtEnd
        alignment="bottom"
        maintainVisibleContentPosition={{ autoscrollToBottomThreshold: 40 }}
        estimatedItemSize={120}
        style={{ flex: 1 }}
        contentStyle={{ paddingHorizontal: 20, paddingVertical: 14, gap: 10 }}
        viewabilityConfig={{ itemVisibleThreshold: 0.2 }}
        onViewableItemsChange={({ viewableItems }) => {
          if (viewableItems.length > 0) {
            visibleRange.set(
              `${viewableItems[0].index}..${viewableItems[viewableItems.length - 1].index}`,
            );
          }
        }}
      >
        {(item) => (
          <View style={{ width: "100%", maxWidth: 760, alignSelf: "center" }}>
            <If
              when={item().kind === "streaming"}
              else={
                <If
                  when={item().kind === "user"}
                  else={
                    <If
                      when={item().kind === "tool"}
                      else={
                        <View
                          selectable
                          style={{
                            width: "100%",
                            padding: 12,
                            borderRadius: 10,
                            backgroundColor: "#111114",
                            borderWidth: 1,
                            borderColor: "#1f1f23",
                          }}
                        >
                          <TextView
                            markdown={item().text}
                            selectable
                            extensions={MERMAID_EXTENSIONS}
                            renderCustom={renderMermaidCustom as any}
                            styles={MARKDOWN_STYLES}
                            style={{ width: "100%", minWidth: 0 }}
                          />
                        </View>
                      }
                    >
                      <View
                        style={{
                          width: "100%",
                          paddingHorizontal: 12,
                          paddingVertical: 8,
                          borderRadius: 8,
                          backgroundColor: "#141417",
                          borderWidth: 1,
                          borderColor: "#27272a",
                          flexDirection: "row",
                          justifyContent: "space-between",
                        }}
                      >
                        <Text style={{ fontSize: 12, color: "#d4d4d8" }}>{item().toolTitle ?? ""}</Text>
                        <Text style={{ fontSize: 12, color: "#71717a" }}>{item().text}</Text>
                      </View>
                    </If>
                  }
                >
                  <View style={{ width: "100%", alignItems: "flex-end" }}>
                    <View
                      style={{
                        maxWidth: "80%",
                        paddingHorizontal: 14,
                        paddingVertical: 10,
                        borderRadius: 12,
                        backgroundColor: "#27272a",
                      }}
                    >
                      <Text style={{ fontSize: 14, lineHeight: 20, color: "#fafafa" }}>{item().text}</Text>
                    </View>
                  </View>
                </If>
              }
            >
              <View
                selectable
                style={{
                  width: "100%",
                  padding: 12,
                  borderRadius: 10,
                  backgroundColor: "#18181b",
                  borderWidth: 1,
                  borderColor: "#27272a",
                }}
              >
                <TextView
                  markdown={streamText()}
                  options={{ streaming: streaming() }}
                  selectable
                  extensions={MERMAID_EXTENSIONS}
                  renderCustom={renderMermaidCustom as any}
                  styles={MARKDOWN_STYLES}
                  style={{ width: "100%", minWidth: 0 }}
                />
              </View>
            </If>
          </View>
        )}
      </List>

      {/* 底栏多行富输入：验证 multiline / Shift+Enter / onPaste / onCaretMove / IME 接线 */}
      <View
        style={{
          paddingHorizontal: 20,
          paddingVertical: 12,
          borderTopWidth: 1,
          borderTopColor: "#27272a",
          backgroundColor: "#111114",
        }}
      >
        <View
          style={{
            width: "100%",
            maxWidth: 760,
            alignSelf: "center",
            borderRadius: 10,
            borderWidth: 1,
            borderColor: "#3f3f46",
            backgroundColor: "#18181b",
            paddingHorizontal: 12,
            paddingVertical: 8,
          }}
        >
          <Input
            multiline
            submitBehavior="submit"
            pasteBehavior="report"
            value={draft()}
            placeholder="输入消息（Enter 发送，Shift+Enter 换行，支持粘贴图片）"
            onChange={({ value }) => draft.set(value)}
            onSubmit={() => draft.set("")}
            onCaretMove={(ev: any) => {
              if (ev?.pageBounds) {
                caretInfo.set(`caret@${Math.round(ev.pageBounds.x)},${Math.round(ev.pageBounds.y)}`);
              }
            }}
            onPaste={(ev: any) => {
              pasteInfo.set(`pasted:${ev?.files?.length ?? 0}f`);
            }}
            onContentSizeChange={({ size }: any) => {
              if (size?.height) {
                const next = Math.max(44, Math.min(180, Math.ceil(size.height)));
                if (next !== inputHeight()) {
                  inputHeight.set(next);
                }
              }
            }}
            style={{
              width: "100%",
              height: inputHeight(),
              fontSize: 14,
              color: "#fafafa",
              placeholderColor: "#71717a",
            }}
          />
        </View>
      </View>
    </View>
  );
}
