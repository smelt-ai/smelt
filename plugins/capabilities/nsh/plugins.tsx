// 能力库「技能」「扩展」页签：扫描目录、导入、移至通用目录 / 同名择一、给智能体用、删除。

import { computed } from "nsh";
import { SmeltButton, t } from "smelt/sdk";
import {
  derivePluginRoots,
  formatUsedByLine,
  isPluginUsable,
  pluginLocation,
  summarizePlugins,
} from "../model.js";
import { agentSettingsOn, type PageController } from "./state.ts";
import {
  DropdownMenuButton,
  MenuEntry,
  PageIntro,
  SectionCell,
  SettingsPageShell,
  SettingsRow,
  SettingsSection,
  StatusLine,
  UsesPopover,
} from "./ui.tsx";

export function PluginsTab(props: { page: PageController; kind: "skill" | "extension" }) {
  const cfg = computed(() => props.page.piConfig());
  const summary = computed(() => summarizePlugins(cfg()?.plugins ?? [], props.kind, t));
  const roots = computed(() => derivePluginRoots(props.kind, cfg()?.paths));

  const importLabel = () =>
    props.kind === "skill" ? t("plugins.import_skill") : t("plugins.import_extension");
  const importDesc = () =>
    props.kind === "skill"
      ? t("plugins.import_description.skill")
      : t("plugins.import_description.extension");
  const sourcesDesc = () =>
    props.kind === "skill"
      ? t("plugins.sources_description.skill")
      : t("plugins.sources_description.extension");

  return (
    <SettingsPageShell>
      <PageIntro text={summary().summary} />

      {/* 来源与导入 */}
      <SettingsSection title={t("plugins.sources")} description={sourcesDesc()}>
        <SectionCell>
          <SettingsRow title={t("plugins.scan")} description={roots().join("\n")}>
            <SmeltButton
              quiet={true}
              small={true}
              label={t("plugins.rescan")}
              onPress={() => props.page.refreshPlugins()}
            />
          </SettingsRow>
        </SectionCell>
        <SectionCell divider={true}>
          <SettingsRow title={t("plugins.import_confirm")} description={importDesc()}>
            <SmeltButton
              small={true}
              label={importLabel()}
              onPress={() => props.page.importPlugin(props.kind)}
            />
          </SettingsRow>
        </SectionCell>
      </SettingsSection>

      {/* 最近一次操作错误 */}
      <If when={props.page.pluginError() != null}>
        <SettingsSection title={t("plugins.last_failed")}>
          <SectionCell>
            <StatusLine text={props.page.pluginError() ?? ""} error={true} />
          </SectionCell>
        </SettingsSection>
      </If>

      {/* 已安装列表 */}
      <SettingsSection
        title={t("plugins.installed")}
        description={summary().items.length > 0 ? t("plugins.installed_description") : undefined}
      >
        <If when={props.page.agentEditError() != null}>
          <SectionCell>
            <StatusLine
              text={t("uses.failed", { error: props.page.agentEditError() ?? "" })}
              error={true}
            />
          </SectionCell>
        </If>

        <If
          when={summary().items.length > 0}
          else={
            <SectionCell divider={props.page.agentEditError() != null}>
              <StatusLine
                text={
                  cfg() == null
                    ? props.page.pendingText()
                    : props.kind === "skill"
                      ? t("plugins.none.skill")
                      : t("plugins.none.extension")
                }
                error={cfg() == null && props.page.piConfigError() != null}
              />
            </SectionCell>
          }
        >
          <Each of={summary().items} key={(plugin: any) => plugin.id}>
            {(plugin: () => any, ix: () => number) => {
              const needsDivider = () => ix() > 0 || props.page.agentEditError() != null;
              const usable = () => ({ kind: "plugin" as const, id: plugin().id });
              const canUse = () => isPluginUsable(plugin());
              const note = () => {
                const p = plugin();
                const usedBy = formatUsedByLine(
                  agentSettingsOn(props.page.machine()),
                  usable(),
                  t,
                );
                const loc = pluginLocation(p, t);
                const lines = [usedBy ? `${loc} · ${usedBy}` : loc];
                if (p.broken) {
                  lines.push(t("plugins.broken", { reason: p.broken }));
                }
                if (p.legacy_location && (!p.duplicates || p.duplicates.length === 0)) {
                  lines.push(t("plugins.legacy"));
                }
                if (p.duplicates && p.duplicates.length > 0) {
                  lines.push(t("plugins.duplicates", { count: p.duplicates.length }));
                }
                return lines.join("\n");
              };
              const showMove = () =>
                plugin().kind === "skill" &&
                !plugin().builtin &&
                (!plugin().duplicates || plugin().duplicates.length === 0) &&
                Boolean(plugin().legacy_location);
              const showPickCopy = () =>
                plugin().kind === "skill" &&
                !plugin().builtin &&
                Array.isArray(plugin().duplicates) &&
                plugin().duplicates.length > 0;
              const copies = () => [plugin().path, ...(plugin().duplicates ?? [])];

              return (
                <SectionCell divider={needsDivider()}>
                  <SettingsRow
                    title={plugin().name}
                    description={plugin().description ? plugin().description : null}
                    clampDescription={plugin().description ? 2 : undefined}
                    note={note()}
                  >
                    <If when={showMove()}>
                      <SmeltButton
                        small={true}
                        label={t("plugins.move")}
                        onPress={() => props.page.adoptPlugin(plugin().id, plugin().path)}
                      />
                    </If>
                    <If when={showPickCopy()}>
                      <DropdownMenuButton
                        label={t("plugins.pick_copy")}
                        align="end"
                        minWidth={220}
                      >
                        <Each of={copies()} key={(path: string) => path}>
                          {(path: () => string) => (
                            <MenuEntry
                              label={path()}
                              onSelect={() => props.page.adoptPlugin(plugin().id, path())}
                            />
                          )}
                        </Each>
                      </DropdownMenuButton>
                    </If>
                    <If when={canUse()}>
                      <UsesPopover page={props.page} usable={usable()} />
                    </If>
                    <If when={!plugin().builtin}>
                      <SmeltButton
                        quiet={true}
                        small={true}
                        label={t("common.delete")}
                        onPress={() => props.page.requestDeletePlugin(plugin())}
                      />
                    </If>
                  </SettingsRow>
                </SectionCell>
              );
            }}
          </Each>
        </If>
      </SettingsSection>
    </SettingsPageShell>
  );
}
