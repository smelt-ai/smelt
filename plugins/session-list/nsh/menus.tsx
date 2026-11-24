// 会话列表的菜单与确认框：使用 NSH 内置 Menu / MenuItem / MenuSeparator / Submenu / Dialog / Tooltip，
// 外观跟着宿主主题走，零硬编码颜色。

import { Image, signal, Text, View } from "nsh";
import { Dialog, Menu, MenuItem, MenuSeparator, Submenu, Tooltip } from "nsh/base";
import {
  DIALOG_SURFACE_CLASS,
  MENU_ITEM_CLASS,
  POPUP_CLASS,
  SmeltButton,
  SmeltIcon,
  TOOLTIP_CLASS,
} from "smelt/sdk";
export { MENU_ITEM_CLASS, POPUP_CLASS };

import { MACHINE_MENU } from "../contract.js";
import { icon, message } from "../util.js";
import {
  ContributedMenuItem,
  fetchContextMenuItems,
  hostAdapter,
  list,
  space,
  t,
  theme,
  tick,
} from "./bridge.ts";

export interface ConfirmRequest {
  title: string;
  action: string;
  description: string;
  danger: boolean;
  confirm: () => void;
}

export function Tip(props: { text: string }) {
  const look = () => theme();
  return (
    <Tooltip class={TOOLTIP_CLASS}>
      <Text
        style={{
          color: look().palette.text,
          fontSize: look().type.text_xsmall.size,
          lineHeight: look().type.text_xsmall.line_height,
        }}
      >
        {props.text}
      </Text>
    </Tooltip>
  );
}

export function MenuEntry(props: {
  label: string;
  image?: string | null;
  iconPath?: string | null;
  iconColor?: string;
  checked?: boolean;
  danger?: boolean;
  disabled?: boolean;
  closeOnSelect?: boolean;
  onSelect: () => void;
}) {
  const look = () => theme();
  const color = () => (props.danger ? look().palette.bad : look().palette.text);
  const iconColor = () =>
    props.iconColor ?? (props.danger ? look().palette.bad : look().palette.muted);
  return (
    <MenuItem
      checked={props.checked}
      disabled={props.disabled}
      closeOnSelect={props.closeOnSelect ?? true}
      onSelect={props.onSelect}
      class={MENU_ITEM_CLASS}
      style={{ height: 28, gap: space(2) }}
    >
      {props.image ? (
        <View style={{ width: 14, height: 14, flexShrink: 0 }}>
          <Image src={props.image} style={{ width: 14, height: 14, objectFit: "contain" }} />
        </View>
      ) : props.iconPath ? (
        <View style={{ width: 14, height: 14, flexShrink: 0 }}>
          <SmeltIcon path={props.iconPath} size={14} color={iconColor()} />
        </View>
      ) : null}
      <Text
        class="flex-1 min-w-0"
        style={{
          color: color(),
          fontSize: look().type.ui.size,
          lineHeight: look().type.ui.line_height,
          numberOfLines: 1,
        }}
      >
        {props.label}
      </Text>
      {props.checked != null ? (
        <View
          style={{
            width: 14,
            height: 14,
            alignItems: "center",
            justifyContent: "center",
            flexShrink: 0,
          }}
        >
          {props.checked ? (
            <SmeltIcon path={icon("check")} size={13} color={look().palette.text} />
          ) : null}
        </View>
      ) : null}
    </MenuItem>
  );
}

export function MenuDivider() {
  return <MenuSeparator class="my-1 bg-border" style={{ height: 1 }} />;
}

export function MenuSectionTitle(props: { text: string }) {
  const look = () => theme();
  return (
    <View style={{ paddingHorizontal: space(2), paddingVertical: space(1) }}>
      <Text
        style={{
          color: look().palette.faint,
          fontSize: look().type.small.size,
          lineHeight: look().type.small.line_height,
          fontWeight: look().type.small_medium.weight,
        }}
      >
        {props.text}
      </Text>
    </View>
  );
}

/** 其它插件往右键菜单登记的项（含子菜单）。 */
export function ContributedMenuItems(props: {
  items: ContributedMenuItem[];
  leadingDivider?: boolean;
  onFail?: (command: string, err: unknown) => void;
}) {
  const look = () => theme();
  return (
    <>
      {(props.leadingDivider ?? true) && props.items.length > 0 ? <MenuDivider /> : null}
      <Each of={props.items} key={(item: ContributedMenuItem, ix: number) => `${item.label}:${ix}`}>
        {(item: () => ContributedMenuItem) =>
          item().submenu && item().submenu!.length > 0 ? (
            <Submenu
              popupClass={POPUP_CLASS}
              popupStyle={{ minWidth: 168 }}
              trigger={
                <View class={MENU_ITEM_CLASS} style={{ height: 28, gap: space(2) }}>
                  {item().image ? (
                    <View style={{ width: 14, height: 14, flexShrink: 0 }}>
                      <Image
                        src={item().image!}
                        style={{ width: 14, height: 14, objectFit: "contain" }}
                      />
                    </View>
                  ) : item().icon ? (
                    <View style={{ width: 14, height: 14, flexShrink: 0 }}>
                      <SmeltIcon path={item().icon!} size={14} color={look().palette.muted} />
                    </View>
                  ) : null}
                  <Text
                    class="flex-1 min-w-0"
                    style={{
                      color: look().palette.text,
                      fontSize: look().type.ui.size,
                      lineHeight: look().type.ui.line_height,
                      numberOfLines: 1,
                    }}
                  >
                    {item().label}
                  </Text>
                  <SmeltIcon path={icon("chevron-right")} size={12} color={look().palette.muted} />
                </View>
              }
            >
              <ContributedMenuItems
                items={item().submenu!}
                leadingDivider={false}
                onFail={props.onFail}
              />
            </Submenu>
          ) : (
            <MenuEntry
              label={item().label}
              image={item().image}
              iconPath={item().icon}
              onSelect={() => {
                const current = item();
                if (!current.command) return;
                hostAdapter.run(current.command, current.args ?? null).catch((err: unknown) => {
                  props.onFail?.(current.command!, err);
                });
              }}
            />
          )
        }
      </Each>
    </>
  );
}

export interface MachineMenuGroup {
  machine: string | null;
  name: string;
  os_icon: string;
  light: string;
  items: ContributedMenuItem[];
}

export function fetchMachinesMenuGroups(machines: any[]): Promise<MachineMenuGroup[]> {
  return Promise.all(
    machines.map((info: any) =>
      fetchContextMenuItems(MACHINE_MENU, info.machine, { machine: info.machine }).then((items) => ({
        machine: info.machine,
        name: info.name,
        os_icon: info.os_icon,
        light: info.link?.light ?? "good",
        items,
      })),
    ),
  ).then((groups) => groups.filter((g) => g.items.length > 0));
}

function machineLightColor(light: string): string {
  const p = theme().palette;
  if (light === "good") return p.ok;
  if (light === "degraded") return p.need;
  return p.bad;
}

/** 渲染「项目」标题上的机器菜单内容：有远程机器时每台一个子菜单，只有本机时直接平铺。 */
export function MachinesMenuEntries(props: {
  groups: MachineMenuGroup[];
  hasRemote: boolean;
}) {
  const look = () => theme();
  const reportFail = (command: string, err: unknown) => {
    list.notify_error(list.failed(t("action.run_command", { command }), message(err)));
  };
  return (
    <If
      when={props.hasRemote}
      else={
        <ContributedMenuItems
          items={props.groups[0]?.items ?? []}
          leadingDivider={false}
          onFail={reportFail}
        />
      }
    >
      <Each of={props.groups} key={(g: MachineMenuGroup) => g.machine ?? "local"}>
        {(g: () => MachineMenuGroup) => (
          <Submenu
            popupClass={POPUP_CLASS}
            popupStyle={{ minWidth: 168 }}
            trigger={
              <View class={MENU_ITEM_CLASS} style={{ height: 28, gap: space(2) }}>
                <View style={{ width: 14, height: 14, flexShrink: 0 }}>
                  <SmeltIcon
                    path={g().os_icon}
                    size={14}
                    color={machineLightColor(g().light)}
                  />
                </View>
                <Text
                  class="flex-1 min-w-0"
                  style={{
                    color: look().palette.text,
                    fontSize: look().type.ui.size,
                    lineHeight: look().type.ui.line_height,
                    numberOfLines: 1,
                  }}
                >
                  {g().name}
                </Text>
                <SmeltIcon path={icon("chevron-right")} size={12} color={look().palette.muted} />
              </View>
            }
          >
            <ContributedMenuItems
              items={g().items}
              leadingDivider={false}
              onFail={reportFail}
            />
          </Submenu>
        )}
      </Each>
    </If>
  );
}

/** 「筛选与分组」菜单。 */
export function GroupingMenu(props: { onOpenChange?: (open: boolean) => void }) {
  const look = () => theme();
  const open = signal(false);
  const grouping = () => {
    tick();
    return list.ui.grouping;
  };
  const hideEmpty = () => {
    tick();
    return list.ui.hide_empty_projects;
  };
  const byMachine = () => {
    tick();
    return list.ui.by_machine;
  };
  const hasRemote = () => {
    tick();
    return list.sidebar.has_remote();
  };
  const active = () => grouping() !== "project" || hideEmpty() || byMachine();

  return (
    <Menu
      placement="bottom"
      align="end"
      popupClass={POPUP_CLASS}
      popupStyle={{ minWidth: 176 }}
      onOpenChange={({ open: next }) => {
        open.set(next);
        props.onOpenChange?.(next);
      }}
      trigger={
        <View
          role="button"
          accessibilityLabel={t("list.grouping_menu")}
          tooltip={<Tip text={t("list.grouping_menu")} />}
          class={`items-center justify-center rounded-(--radius) ${
            open() ? "bg-accent" : "hover:bg-accent"
          }`}
          style={{ width: 24, height: 24, cursor: "default" }}
        >
          <SmeltIcon
            path={icon("sliders-horizontal")}
            size={14}
            color={active() ? look().palette.text : look().palette.muted}
          />
        </View>
      }
    >
      <MenuSectionTitle text={t("grouping.title")} />
      <MenuEntry
        label={t("grouping.project")}
        checked={grouping() === "project"}
        onSelect={() => list.set_grouping("project")}
      />
      <MenuEntry
        label={t("grouping.status")}
        checked={grouping() === "status"}
        onSelect={() => list.set_grouping("status")}
      />
      <MenuEntry
        label={t("grouping.time")}
        checked={grouping() === "last_updated"}
        onSelect={() => list.set_grouping("last_updated")}
      />
      <MenuDivider />
      <MenuSectionTitle text={t("grouping.show")} />
      <MenuEntry
        label={t("grouping.all")}
        checked={!hideEmpty()}
        onSelect={() => list.set_hide_empty_projects(false)}
      />
      <MenuEntry
        label={t("grouping.with_sessions")}
        checked={hideEmpty()}
        onSelect={() => list.set_hide_empty_projects(true)}
      />
      {hasRemote() ? (
        <>
          <MenuDivider />
          <MenuEntry
            label={t("grouping.by_machine")}
            checked={byMachine()}
            onSelect={() => list.set_by_machine(!byMachine())}
          />
        </>
      ) : null}
    </Menu>
  );
}

/** 关闭运行中会话、非空项目或工作树前的确认对话框。 */
export function ConfirmDialog(props: {
  request: () => ConfirmRequest | null;
  onDismiss: () => void;
}) {
  const look = () => theme();
  const req = props.request;
  return (
    <Dialog
      open={req() != null}
      role="alertdialog"
      animation="fade"
      accessibilityLabel={req()?.title ?? ""}
      accessibilityDescription={req()?.description ?? ""}
      backdropClass="bg-black/40"
      onOpenChange={({ open }) => {
        if (!open) props.onDismiss();
      }}
      class={`m-auto ${DIALOG_SURFACE_CLASS} p-4`}
      style={{ width: 340, gap: space(3) }}
    >
      {req() ? (
        <>
          <Text
            style={{
              color: look().palette.text,
              fontSize: look().type.ui_medium.size,
              lineHeight: look().type.ui_medium.line_height,
              fontWeight: look().type.ui_medium.weight,
            }}
          >
            {req()!.title}
          </Text>
          {req()!.description ? (
            <Text
              style={{
                color: look().palette.muted,
                fontSize: look().type.text_xsmall.size,
                lineHeight: look().type.text_xsmall.line_height,
              }}
            >
              {req()!.description}
            </Text>
          ) : null}
          <View class="flex-row items-center justify-end" style={{ gap: space(2) }}>
            <SmeltButton
              label={t("action.cancel")}
              quiet={true}
              small={true}
              onPress={props.onDismiss}
            />
            <SmeltButton
              label={req()!.action}
              primary={!req()!.danger}
              danger={req()!.danger}
              small={true}
              onPress={() => {
                const fn = req()!.confirm;
                props.onDismiss();
                fn();
              }}
            />
          </View>
        </>
      ) : null}
    </Dialog>
  );
}
