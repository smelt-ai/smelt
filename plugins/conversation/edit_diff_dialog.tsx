// 点击改动标签弹出的文件差异弹窗：文件标签栏 + 一等 Rust `<SmeltDiffView>` +「在文件里打开」。
import { computed, effect, Scroll, signal, Text, View } from "nsh";
import { openFile, SmeltButton, SmeltDialog, SmeltDiffView, SmeltIcon, theme } from "smelt/sdk";
import { ct } from "./i18n.ts";
import { relativePathToCwd } from "./model.js";

export interface EditChunk {
  old: string | null;
  new: string;
}

export interface EditFileItem {
  path: string;
  label: string;
  added: number;
  removed: number;
  edits: EditChunk[];
}

export interface EditDiffDialogState {
  files: EditFileItem[];
  selected: number;
  cwd: string | null;
  machine: string | null;
}

function languageForPath(path: string): string | undefined {
  const dot = path.lastIndexOf(".");
  if (dot < 0) return undefined;
  const ext = path.slice(dot + 1).toLowerCase();
  const map: Record<string, string> = {
    rs: "rust",
    ts: "typescript",
    tsx: "tsx",
    js: "javascript",
    jsx: "jsx",
    mjs: "javascript",
    json: "json",
    yml: "yaml",
    yaml: "yaml",
    toml: "toml",
    md: "markdown",
    py: "python",
    go: "go",
    sh: "bash",
    zsh: "bash",
    css: "css",
    html: "html",
    sql: "sql",
    swift: "swift",
    dart: "dart",
    c: "c",
    h: "c",
    cpp: "cpp",
    hpp: "cpp",
    java: "java",
  };
  return map[ext];
}

export function EditDiffDialog(props: {
  dialog: () => EditDiffDialogState | null;
  onClose: () => void;
}) {
  const look = theme;
  const selectedIdx = signal(0);
  const openError = signal<string | null>(null);

  effect(() => {
    const d = props.dialog();
    if (d) {
      selectedIdx.set(Math.max(0, Math.min(d.selected, Math.max(0, d.files.length - 1))));
      openError.set(null);
    }
  });

  const currentFile = computed(() => {
    const d = props.dialog();
    if (!d || d.files.length === 0) return null;
    const idx = Math.max(0, Math.min(selectedIdx(), d.files.length - 1));
    return d.files[idx] ?? null;
  });

  const visibleEdits = computed(() => {
    const f = currentFile();
    if (!f) return [];
    return f.edits.filter((e) => (e.old ?? "") !== e.new);
  });

  const handleOpenInFile = () => {
    const d = props.dialog();
    const f = currentFile();
    if (!d || !f) return;
    openError.set(null);
    openFile({ machine: d.machine, path: f.path, line: null }).catch((err: any) => {
      openError.set(err?.message ?? String(err));
    });
  };

  return (
    <SmeltDialog
      open={props.dialog() != null}
      backdropClass="bg-black/45"
      onOpenChange={({ open }: any) => {
        if (!open) props.onClose();
      }}
      class="m-auto"
    >
      <If when={props.dialog() != null}>
        <View
          class="flex-col rounded-xl border border-border bg-background shadow-xl overflow-hidden"
          style={{
            width: 920,
            maxWidth: "92%",
            height: 620,
            maxHeight: "82%",
          }}
        >
          {/* 顶栏：当前文件路径 + 在文件里打开 + 关闭 */}
          <View
            style={{
              height: 44,
              paddingHorizontal: 14,
              flexDirection: "row",
              alignItems: "center",
              gap: 8,
              borderBottomWidth: 1,
              borderBottomColor: look().palette.hair,
              backgroundColor: look().palette.surface,
              flexShrink: 0,
            }}
          >
            <SmeltIcon path="icons/file.svg" size={14} color={look().palette.muted} />
            <Text
              style={{
                flex: 1,
                minWidth: 0,
                fontSize: look().type.ui_medium.size,
                fontWeight: look().type.ui_medium.weight,
                color: look().palette.text,
                numberOfLines: 1,
              }}
            >
              {currentFile()
                ? relativePathToCwd(currentFile()!.path, props.dialog()?.cwd ?? null)
                : ct("diff.title")}
            </Text>
            <If when={Boolean(openError())}>
              <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.bad }}>
                {openError() ?? ""}
              </Text>
            </If>
            <SmeltButton
              quiet
              small
              icon="icons/external-link.svg"
              label={ct("action.open_in_file")}
              onPress={handleOpenInFile}
            />
            <SmeltButton
              quiet
              small
              icon="icons/close.svg"
              accessibilityLabel={ct("action.close")}
              onPress={props.onClose}
            />
          </View>

          {/* 多文件标签条 */}
          <If when={(props.dialog()?.files.length ?? 0) > 1}>
            <Scroll
              horizontal
              style={{
                flexShrink: 0,
                borderBottomWidth: 1,
                borderBottomColor: look().palette.hair,
                backgroundColor: look().palette.surface,
              }}
            >
              <View style={{ flexDirection: "row", alignItems: "center", gap: 4, padding: 6 }}>
                <Each of={props.dialog()?.files ?? []}>
                  {(file, index) => {
                    const active = computed(() => selectedIdx() === index());
                    return (
                      <View
                        onPress={() => selectedIdx.set(index())}
                        style={{
                          paddingHorizontal: 10,
                          paddingVertical: 5,
                          borderRadius: look().radius.sm ?? 6,
                          flexDirection: "row",
                          alignItems: "center",
                          gap: 6,
                          backgroundColor: active() ? look().palette.row_selected : "transparent",
                        }}
                      >
                        <Text
                          style={{
                            fontSize: look().type.small.size,
                            color: active() ? look().palette.text : look().palette.muted,
                          }}
                        >
                          {file().label}
                        </Text>
                        <If when={file().added > 0}>
                          <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.ok }}>
                            +{file().added}
                          </Text>
                        </If>
                        <If when={file().removed > 0}>
                          <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.bad }}>
                            -{file().removed}
                          </Text>
                        </If>
                      </View>
                    );
                  }}
                </Each>
              </View>
            </Scroll>
          </If>

          {/* 差异主体：一等 Rust <SmeltDiffView> */}
          <If
            when={visibleEdits().length > 0}
            else={
              <View style={{ flex: 1, alignItems: "center", justifyContent: "center", padding: 24 }}>
                <Text style={{ fontSize: look().type.ui.size, color: look().palette.muted }}>
                  {ct("diff.no_changes")}
                </Text>
              </View>
            }
          >
            <If
              when={visibleEdits().length === 1}
              else={
                <Scroll style={{ flex: 1, width: "100%", minHeight: 0 }}>
                  <View style={{ padding: 12, gap: 14 }}>
                    <Each of={visibleEdits()}>
                      {(chunk, idx) => (
                        <View
                          style={{
                            borderRadius: look().radius.md ?? 8,
                            borderWidth: 1,
                            borderColor: look().palette.hair,
                            overflow: "hidden",
                          }}
                        >
                          <View
                            style={{
                              paddingHorizontal: 10,
                              paddingVertical: 6,
                              backgroundColor: look().palette.surface,
                              borderBottomWidth: 1,
                              borderBottomColor: look().palette.hair,
                            }}
                          >
                            <Text style={{ fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
                              {ct("diff.nth_edit", { n: idx() + 1 })}
                            </Text>
                          </View>
                          <View style={{ height: 280 }}>
                            <SmeltDiffView
                              oldValue={chunk().old ?? ""}
                              newValue={chunk().new}
                              language={currentFile() ? languageForPath(currentFile()!.path) : undefined}
                              layout="unified"
                              lineNumbers={false}
                            />
                          </View>
                        </View>
                      )}
                    </Each>
                  </View>
                </Scroll>
              }
            >
              <View style={{ flex: 1, minHeight: 0 }}>
                <SmeltDiffView
                  oldValue={visibleEdits()[0]?.old ?? ""}
                  newValue={visibleEdits()[0]?.new ?? ""}
                  language={currentFile() ? languageForPath(currentFile()!.path) : undefined}
                  layout="unified"
                  lineNumbers={false}
                />
              </View>
            </If>
          </If>
        </View>
      </If>
    </SmeltDialog>
  );
}
