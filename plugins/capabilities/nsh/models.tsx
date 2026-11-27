// 设置「模型」页：默认模型、推理强度、服务商凭据与自定义服务商、原生配置文件。

import { computed, Text, View } from "nsh";
import { SmeltButton, SmeltInput, SmeltMenuItem, SmeltMenuSeparator, t } from "smelt/sdk";
import {
  authStatusConfigured,
  authStatusNote,
  buildModelGroups,
  customProviderNote,
  discoveredModelLabel,
  formatModelLabel,
  isDefaultProviderSignedOut,
  mergeProviders,
  nonEmptyOr,
  PI_THINKING_LEVELS,
  PROVIDER_API_PROTOCOLS,
  thinkingLevelLabel,
} from "../model.js";
import { LoginPanel } from "./auth.tsx";
import {
  currentTheme,
  type CustomModelRowDraft,
  type CustomProviderDraft,
  type ManualModelDraft,
  type PageController,
} from "./state.ts";
import {
  BadgePill,
  DropdownMenuButton,
  MenuSub,
  SectionCell,
  SettingsPageShell,
  SettingsRow,
  SettingsSection,
  StatusLine,
  TextField,
} from "./ui.tsx";

export function ModelsTab(props: { page: PageController }) {
  const cfg = computed(() => props.page.piConfig());
  const failed = computed(() => cfg() == null && props.page.piConfigError() != null);

  return (
    <SettingsPageShell>
      <If
        when={cfg() != null}
        else={
          <SettingsSection>
            <SectionCell>
              <StatusLine text={props.page.pendingText()} error={failed()} />
            </SectionCell>
          </SettingsSection>
        }
      >
        <DefaultModelSection page={props.page} />
        <ProvidersSection page={props.page} />
        <NativeFilesSection page={props.page} />
      </If>
    </SettingsPageShell>
  );
}

// ---- 默认模型与推理强度 ----

function DefaultModelSection(props: { page: PageController }) {
  const cfg = computed(() => props.page.piConfig());
  const modelResult = computed(() => cfg()?.modelSettings ?? { ok: false, error: "" });
  const settings = computed(() => (modelResult().ok ? (modelResult() as any).value : null));
  const builtin = computed(() => {
    const auth = props.page.authProviders();
    return auth?.status === "ready" ? auth.providers : null;
  });
  const groups = computed(() => {
    const s = settings();
    if (!s) return [];
    return buildModelGroups(
      s.customProviders,
      builtin(),
      cfg()?.modelChoiceGroups ?? [],
      s.defaultModel,
    );
  });
  const current = computed(() => settings()?.defaultModel ?? null);
  const label = computed(() => {
    const cur = current();
    return cur ? formatModelLabel(groups(), cur) : t("default_model.auto");
  });
  const signedOut = computed(() => {
    const cur = current();
    const s = settings();
    if (!cur || !s) return false;
    return isDefaultProviderSignedOut(s.customProviders, builtin(), cur);
  });
  const thinking = computed(() => settings()?.thinkingLevel ?? null);
  const thinkingLabel = computed(() => {
    const lv = thinking();
    return lv ? thinkingLevelLabel(lv, t) : t("default_model.thinking_unset");
  });
  const editing = computed(() => props.page.modelEditor() != null);

  return (
    <SettingsSection>
      <If
        when={modelResult().ok}
        else={
          <SectionCell>
            <StatusLine
              text={t("default_model.read_failed", { error: (modelResult() as any).error ?? "" })}
              error={true}
            />
          </SectionCell>
        }
      >
        <SectionCell>
          <SettingsRow
            title={t("default_model.title")}
            description={current() == null ? t("default_model.auto_description") : null}
          >
            <If when={signedOut()}>
              <BadgePill label={t("default_model.signed_out")} tone="need" />
            </If>
            <DropdownMenuButton
              label={label()}
              disabled={editing()}
              align="end"
              minWidth={200}
            >
              <SmeltMenuItem
                label={t("default_model.auto")}
                checked={current() == null}
                onSelect={() => props.page.clearDefaultModel()}
              />
              <SmeltMenuSeparator />
              <If when={groups().length === 0}>
                <SmeltMenuItem label={t("default_model.none")} disabled={true} onSelect={() => {}} />
              </If>
              <Each of={groups()} key={(g: any) => g.provider}>
                {(group: () => any) => {
                  const inGroup = () => current()?.provider === group().provider;
                  return (
                    <MenuSub label={group().name} checked={inGroup()}>
                      <Each of={group().models} key={(m: any) => m.id}>
                        {(m: () => { id: string; name: string }) => (
                          <SmeltMenuItem
                            label={m().name}
                            checked={inGroup() && current()?.model === m().id}
                            onSelect={() => props.page.setDefaultModel(group().provider, m().id)}
                          />
                        )}
                      </Each>
                    </MenuSub>
                  );
                }}
              </Each>
              <SmeltMenuSeparator />
              <SmeltMenuItem
                label={t("default_model.manual_menu")}
                onSelect={() => props.page.openManualModelEditor()}
              />
            </DropdownMenuButton>
          </SettingsRow>
        </SectionCell>

        <If when={props.page.modelEditor() != null}>
          <SectionCell divider={true}>
            <ManualModelForm
              page={props.page}
              draft={() => props.page.modelEditor()!}
            />
          </SectionCell>
        </If>

        <If when={props.page.defaultError() != null}>
          <SectionCell divider={true}>
            <StatusLine
              text={t("common.action_failed", { error: props.page.defaultError() ?? "" })}
              error={true}
            />
          </SectionCell>
        </If>

        <SectionCell divider={true}>
          <SettingsRow title={t("default_model.thinking")}>
            <DropdownMenuButton label={thinkingLabel()} align="end" minWidth={140}>
              <Each of={PI_THINKING_LEVELS} key={(lv: string) => lv}>
                {(lv: () => string) => (
                  <SmeltMenuItem
                    label={thinkingLevelLabel(lv(), t)}
                    checked={thinking() === lv()}
                    onSelect={() => props.page.setThinkingLevel(lv())}
                  />
                )}
              </Each>
            </DropdownMenuButton>
          </SettingsRow>
        </SectionCell>
      </If>
    </SettingsSection>
  );
}

function ManualModelForm(props: {
  page: PageController;
  draft: () => ManualModelDraft;
}) {
  const saving = () => props.page.modelEditorSaving();
  const submit = () => props.page.saveManualModel();
  return (
    <View style={{ display: "flex", flexDirection: "column", width: "100%", gap: 12, paddingVertical: 4 }}>
      <View style={{ display: "flex", flexDirection: "row", alignItems: "flex-start", gap: 12, width: "100%" }}>
        <TextField
          label={t("model_editor.provider")}
          value={props.draft().provider}
          placeholder={t("model_editor.provider_placeholder")}
          disabled={saving()}
          onChange={(provider) => props.page.updateManualModel({ provider })}
          onSubmit={submit}
        />
        <TextField
          label={t("model_editor.model")}
          value={props.draft().model}
          placeholder={t("model_editor.model_placeholder")}
          disabled={saving()}
          onChange={(model) => props.page.updateManualModel({ model })}
          onSubmit={submit}
        />
      </View>
      <View style={{ display: "flex", flexDirection: "row", alignItems: "flex-start", gap: 12, width: "100%" }}>
        <TextField
          label={t("model_editor.base_url")}
          value={props.draft().baseUrl}
          placeholder={t("model_editor.base_url_placeholder")}
          disabled={saving()}
          onChange={(baseUrl) => props.page.updateManualModel({ baseUrl })}
          onSubmit={submit}
        />
        <TextField
          label={t("model_editor.api_key")}
          value={props.draft().apiKey}
          placeholder={t("model_editor.key_placeholder")}
          masked={true}
          disabled={saving()}
          onChange={(apiKey) => props.page.updateManualModel({ apiKey })}
          onSubmit={submit}
        />
      </View>
      <If when={props.page.modelEditorError() != null}>
        <StatusLine text={props.page.modelEditorError() ?? ""} error={true} />
      </If>
      <View style={{ display: "flex", flexDirection: "row", justifyContent: "flex-end", gap: 8 }}>
        <SmeltButton
          variant="quiet"
          size="sm"
          disabled={saving()}
          label={t("common.cancel")}
          onPress={() => props.page.cancelManualModelEditor()}
        />
        <SmeltButton
          variant="primary"
          size="sm"
          loading={saving()}
          disabled={saving()}
          label={t("common.save")}
          onPress={submit}
        />
      </View>
    </View>
  );
}

// ---- 服务商列表与自定义服务商表单 ----

function ProvidersSection(props: { page: PageController }) {
  const cfg = computed(() => props.page.piConfig());
  const settings = computed(() => {
    const res = cfg()?.modelSettings;
    return res && res.ok ? res.value : null;
  });
  const custom = computed(() => settings()?.customProviders ?? []);
  const defaultProvider = computed(() => settings()?.defaultModel?.provider ?? "");
  const authState = computed(() => props.page.authProviders());
  const builtin = computed(() => {
    const a = authState();
    return a?.status === "ready" ? a.providers : [];
  });
  const autoIds = computed(() => cfg()?.autoProviderIds ?? []);
  const merged = computed(() =>
    mergeProviders(builtin(), custom(), defaultProvider(), props.page.authShowAll()),
  );
  const providerLogin = computed(() => {
    const view = props.page.loginView();
    return view && view.target.kind === "provider" ? view : null;
  });
  const loginRunning = computed(() => {
    const view = providerLogin();
    return view != null && view.error == null;
  });
  const busy = computed(
    () =>
      loginRunning() ||
      props.page.customProviderEditor() != null ||
      props.page.confirmDialog() != null,
  );
  const orphanLogin = computed(() => {
    const view = providerLogin();
    if (!view) return null;
    const id = view.target.id;
    const visible = merged().entries.some((e: any) => e.id === id);
    return visible ? null : view;
  });
  const addingCustom = computed(() => {
    const ed = props.page.customProviderEditor();
    return ed != null && ed.previousId == null;
  });

  return (
    <SettingsSection title={t("providers.title")}>
      <If when={props.page.authError() != null}>
        <SectionCell>
          <StatusLine
            text={t("common.action_failed", { error: props.page.authError() ?? "" })}
            error={true}
          />
        </SectionCell>
      </If>

      <Each of={merged().entries} key={(entry: any) => `${entry.kind}:${entry.id}`}>
        {(entry: () => any, ix: () => number) => {
          const needsDivider = () => ix() > 0 || props.page.authError() != null;
          const isLoginTarget = () => providerLogin()?.target.id === entry().id;
          const isEditingCustom = () => {
            const ed = props.page.customProviderEditor();
            return ed != null && ed.previousId === entry().id;
          };
          return (
            <>
              <SectionCell divider={needsDivider()}>
                <If
                  when={entry().kind === "builtin"}
                  else={
                    <CustomProviderRow
                      page={props.page}
                      provider={() => entry().provider}
                      isAuto={() => autoIds().includes(entry().id)}
                      busy={busy}
                    />
                  }
                >
                  <BuiltinProviderRow
                    page={props.page}
                    provider={() => entry().provider}
                    busy={busy}
                  />
                </If>
              </SectionCell>
              <If when={isLoginTarget()}>
                <SectionCell divider={true}>
                  <LoginPanel page={props.page} view={() => providerLogin()!} />
                </SectionCell>
              </If>
              <If when={isEditingCustom()}>
                <SectionCell divider={true}>
                  <CustomProviderForm
                    page={props.page}
                    draft={() => props.page.customProviderEditor()!}
                  />
                </SectionCell>
              </If>
            </>
          );
        }}
      </Each>

      {/* 登录中的服务商若被折叠，面板依然显示 */}
      <If when={orphanLogin() != null}>
        <SectionCell divider={merged().entries.length > 0 || props.page.authError() != null}>
          <LoginPanel page={props.page} view={() => orphanLogin()!} />
        </SectionCell>
      </If>

      {/* 新建自定义服务商表单 */}
      <If when={addingCustom()}>
        <SectionCell
          divider={
            merged().entries.length > 0 ||
            props.page.authError() != null ||
            orphanLogin() != null
          }
        >
          <CustomProviderForm
            page={props.page}
            draft={() => props.page.customProviderEditor()!}
          />
        </SectionCell>
      </If>

      {/* 底部：凭据助手状态 / 展开全部 + 添加自定义服务商 */}
      <SectionCell
        divider={
          merged().entries.length > 0 ||
          props.page.authError() != null ||
          orphanLogin() != null ||
          addingCustom()
        }
      >
        <View
          style={{
            display: "flex",
            flexDirection: "row",
            alignItems: "center",
            justifyContent: "space-between",
            width: "100%",
            gap: 12,
          }}
        >
          <View style={{ flex: 1, minWidth: 0 }}>
            <If when={authState() == null || authState()?.status === "loading"}>
              <StatusLine text={t("providers.loading")} />
            </If>
            <If when={authState()?.status === "failed"}>
              <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 8 }}>
                <View style={{ flex: 1, minWidth: 0 }}>
                  <StatusLine
                    text={t("providers.failed", { error: (authState() as any)?.error ?? "" })}
                    error={true}
                  />
                </View>
                <SmeltButton
                  variant="quiet"
                  size="sm"
                  label={t("common.retry")}
                  onPress={() => props.page.refreshAuthProviders()}
                />
              </View>
            </If>
            <If
              when={
                authState()?.status === "ready" &&
                (props.page.authShowAll() || merged().hidden > 0)
              }
            >
              <SmeltButton
                variant="quiet"
                size="sm"
                label={
                  props.page.authShowAll()
                    ? t("providers.show_common")
                    : t("providers.show_all", { hidden: merged().hidden })
                }
                onPress={() => props.page.toggleAuthShowAll()}
              />
            </If>
          </View>
          <SmeltButton
            variant="quiet"
            size="sm"
            icon="icons/plus.svg"
            label={t("provider.add")}
            tooltip={t("provider.add_tooltip")}
            disabled={busy()}
            onPress={() => props.page.openCustomProviderEditor(null)}
          />
        </View>
      </SectionCell>
    </SettingsSection>
  );
}

function BuiltinProviderRow(props: {
  page: PageController;
  provider: () => any;
  busy: () => boolean;
}) {
  const configured = () => authStatusConfigured(props.provider().status);
  const showReauth = () => configured() && props.provider().supportsOauth;
  const showStepByStep = () => props.provider().supportsOauth;
  const showApiKeyMenu = () =>
    props.provider().supportsApiKey && (configured() || props.provider().supportsOauth);
  const apiKeyMenuLabel = () =>
    props.provider().status === "api_key" ? t("auth.change_api_key") : t("auth.use_api_key");
  const hasMore = () => showReauth() || showStepByStep() || showApiKeyMenu() || configured();

  return (
    <SettingsRow title={props.provider().name} note={authStatusNote(props.provider(), t)}>
      <If when={!configured() && props.provider().supportsOauth}>
        <SmeltButton
          size="sm"
          label={t("auth.sign_in")}
          tooltip={t("auth.sign_in_tooltip")}
          disabled={props.busy()}
          onPress={() =>
            props.page.startProviderLogin(
              props.provider().id,
              props.provider().name,
              "oauth",
              false,
            )
          }
        />
      </If>
      <If when={!configured() && !props.provider().supportsOauth && props.provider().supportsApiKey}>
        <SmeltButton
          size="sm"
          label={t("auth.enter_api_key")}
          disabled={props.busy()}
          onPress={() =>
            props.page.startProviderLogin(
              props.provider().id,
              props.provider().name,
              "api_key",
              true,
            )
          }
        />
      </If>
      <If when={hasMore()}>
        <DropdownMenuButton
          iconOnly="icons/ellipsis.svg"
          accessibilityLabel={t("auth.more_actions")}
          disabled={props.busy()}
          align="end"
        >
          <If when={showReauth()}>
            <SmeltMenuItem
              label={t("auth.sign_in_again")}
              onSelect={() =>
                props.page.startProviderLogin(
                  props.provider().id,
                  props.provider().name,
                  "oauth",
                  false,
                )
              }
            />
          </If>
          <If when={showStepByStep()}>
            <SmeltMenuItem
              label={t("auth.sign_in_step_by_step")}
              onSelect={() =>
                props.page.startProviderLogin(
                  props.provider().id,
                  props.provider().name,
                  "oauth",
                  true,
                )
              }
            />
          </If>
          <If when={showApiKeyMenu()}>
            <SmeltMenuItem
              label={apiKeyMenuLabel()}
              onSelect={() =>
                props.page.startProviderLogin(
                  props.provider().id,
                  props.provider().name,
                  "api_key",
                  true,
                )
              }
            />
          </If>
          <If when={configured()}>
            <If when={showReauth() || showStepByStep() || showApiKeyMenu()}>
              <SmeltMenuSeparator />
            </If>
            <SmeltMenuItem
              label={t("auth.sign_out")}
              danger={true}
              onSelect={() =>
                props.page.requestSignOutProvider(props.provider().id, props.provider().name)
              }
            />
          </If>
        </DropdownMenuButton>
      </If>
    </SettingsRow>
  );
}

function CustomProviderRow(props: {
  page: PageController;
  provider: () => any;
  isAuto: () => boolean;
  busy: () => boolean;
}) {
  const title = () => nonEmptyOr(props.provider().displayName, props.provider().id);
  return (
    <SettingsRow
      title={title()}
      note={customProviderNote(props.provider(), props.isAuto(), t)}
    >
      <SmeltButton
        variant="quiet"
        size="sm"
        label={t("provider.edit")}
        disabled={props.busy()}
        onPress={() => props.page.openCustomProviderEditor(props.provider().id)}
      />
      <DropdownMenuButton
        iconOnly="icons/ellipsis.svg"
        accessibilityLabel={t("auth.more_actions")}
        disabled={props.busy()}
        align="end"
      >
        <SmeltMenuItem
          label={t("provider.remove")}
          danger={true}
          onSelect={() => props.page.requestRemoveCustomProvider(props.provider().id, title())}
        />
      </DropdownMenuButton>
    </SettingsRow>
  );
}

function CustomProviderForm(props: {
  page: PageController;
  draft: () => CustomProviderDraft;
}) {
  const look = currentTheme;
  const saving = () => props.page.modelEditorSaving();
  const discovery = () => props.draft().discovery;
  const discovering = () => discovery()?.status === "loading";
  const adoptedIds = computed(() =>
    props
      .draft()
      .models.map((row) => row.id.trim())
      .filter((id) => id.length > 0),
  );
  const title = () =>
    props.draft().previousId
      ? t("provider.edit_title", { id: props.draft().previousId ?? "" })
      : t("provider.add_title");

  return (
    <View style={{ display: "flex", flexDirection: "column", width: "100%", gap: 12, paddingVertical: 4 }}>
      <Text
        style={{
          fontSize: look().type.ui_medium.size,
          lineHeight: look().type.ui_medium.line_height,
          fontWeight: look().type.ui_medium.weight,
          color: look().palette.text,
        }}
      >
        {title()}
      </Text>

      <View style={{ display: "flex", flexDirection: "row", alignItems: "flex-start", gap: 12, width: "100%" }}>
        <TextField
          label={t("provider.id")}
          value={props.draft().id}
          placeholder={t("provider.id_placeholder")}
          disabled={saving()}
          onChange={(id) => props.page.updateCustomProvider({ id })}
        />
        <TextField
          label={t("provider.name")}
          value={props.draft().displayName}
          placeholder={t("provider.name_placeholder")}
          disabled={saving()}
          onChange={(displayName) => props.page.updateCustomProvider({ displayName })}
        />
      </View>

      <View style={{ display: "flex", flexDirection: "row", alignItems: "flex-start", gap: 12, width: "100%" }}>
        <TextField
          label={t("provider.base_url")}
          value={props.draft().baseUrl}
          placeholder="https://gateway.example/v1"
          disabled={saving()}
          onChange={(baseUrl) => props.page.updateCustomProvider({ baseUrl })}
        />
        <View style={{ display: "flex", flexDirection: "column", flexShrink: 0, gap: 4 }}>
          <Text
            style={{
              fontSize: look().type.text_xsmall_medium.size,
              lineHeight: look().type.text_xsmall_medium.line_height,
              fontWeight: look().type.text_xsmall_medium.weight,
              color: look().palette.text,
            }}
          >
            {t("provider.api")}
          </Text>
          <View style={{ height: 28, display: "flex", justifyContent: "center" }}>
            <DropdownMenuButton
              label={props.draft().api}
              disabled={saving()}
              align="end"
              minWidth={210}
            >
              <Each of={PROVIDER_API_PROTOCOLS} key={(proto: string) => proto}>
                {(proto: () => string) => (
                  <SmeltMenuItem
                    label={proto()}
                    checked={props.draft().api === proto()}
                    onSelect={() => props.page.setCustomProviderApi(proto())}
                  />
                )}
              </Each>
            </DropdownMenuButton>
          </View>
        </View>
      </View>

      <TextField
        label={t("provider.api_key")}
        value={props.draft().apiKey}
        placeholder={
          props.draft().credentialConfigured
            ? t("editor.key_configured")
            : t("provider.key_placeholder")
        }
        masked={true}
        disabled={saving()}
        onChange={(apiKey) => props.page.updateCustomProvider({ apiKey })}
      />

      {/* 模型区标题与操作 */}
      <View
        style={{
          display: "flex",
          flexDirection: "row",
          alignItems: "center",
          justifyContent: "space-between",
          width: "100%",
        }}
      >
        <Text
          style={{
            fontSize: look().type.ui_medium.size,
            fontWeight: look().type.ui_medium.weight,
            color: look().palette.text,
          }}
        >
          {t("provider.models")}
        </Text>
        <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 4 }}>
          <SmeltButton
            variant="quiet"
            size="sm"
            label={t("provider.fetch_models")}
            loading={discovering()}
            disabled={discovering() || saving()}
            onPress={() => props.page.discoverCustomModels()}
          />
          <SmeltButton
            variant="quiet"
            size="sm"
            icon="icons/plus.svg"
            label={t("provider.add_model")}
            disabled={saving()}
            onPress={() => props.page.addCustomModelRow()}
          />
        </View>
      </View>

      {/* 模型发现提示与候选按钮 */}
      <ModelDiscoverySection
        page={props.page}
        discovery={discovery}
        adoptedIds={adoptedIds}
        manualRowCount={() => props.draft().models.length}
      />

      {/* 手动模型表 */}
      <If when={props.draft().models.length > 0}>
        <View style={{ display: "flex", flexDirection: "column", width: "100%", gap: 8 }}>
          <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 8, width: "100%" }}>
            <Text style={{ flex: 3, minWidth: 0, fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
              {t("model_row.id")}
            </Text>
            <Text style={{ flex: 3, minWidth: 0, fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
              {t("model_row.name")}
            </Text>
            <Text style={{ flex: 2, minWidth: 0, fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
              {t("model_row.context")}
            </Text>
            <Text style={{ flex: 2, minWidth: 0, fontSize: look().type.text_xsmall.size, color: look().palette.muted }}>
              {t("model_row.max_tokens")}
            </Text>
            <View style={{ width: 24, flexShrink: 0 }} />
          </View>
          <Each of={props.draft().models} key={(row: CustomModelRowDraft) => row.key}>
            {(row: () => CustomModelRowDraft) => (
              <View style={{ display: "flex", flexDirection: "row", alignItems: "center", gap: 8, width: "100%" }}>
                <View style={{ flex: 3, minWidth: 0 }}>
                  <SmeltInput
                    value={row().id}
                    placeholder={t("model_row.id_placeholder")}
                    accessibilityLabel={t("model_row.id")}
                    disabled={saving()}
                    onChange={({ value }) =>
                      props.page.updateCustomModelRow(row().key, { id: value })
                    }
                    class="w-full"
                  />
                </View>
                <View style={{ flex: 3, minWidth: 0 }}>
                  <SmeltInput
                    value={row().name}
                    placeholder={t("model_row.name_placeholder")}
                    accessibilityLabel={t("model_row.name")}
                    disabled={saving()}
                    onChange={({ value }) =>
                      props.page.updateCustomModelRow(row().key, { name: value })
                    }
                    class="w-full"
                  />
                </View>
                <View style={{ flex: 2, minWidth: 0 }}>
                  <SmeltInput
                    value={row().contextWindow}
                    placeholder={t("model_row.context_placeholder")}
                    accessibilityLabel={t("model_row.context")}
                    disabled={saving()}
                    onChange={({ value }) =>
                      props.page.updateCustomModelRow(row().key, { contextWindow: value })
                    }
                    class="w-full"
                  />
                </View>
                <View style={{ flex: 2, minWidth: 0 }}>
                  <SmeltInput
                    value={row().maxTokens}
                    placeholder={t("model_row.max_tokens_placeholder")}
                    accessibilityLabel={t("model_row.max_tokens")}
                    disabled={saving()}
                    onChange={({ value }) =>
                      props.page.updateCustomModelRow(row().key, { maxTokens: value })
                    }
                    class="w-full"
                  />
                </View>
                <View style={{ width: 24, flexShrink: 0, display: "flex", alignItems: "center" }}>
                  <SmeltButton
                    variant="quiet"
                    size="sm"
                    icon="icons/trash.svg"
                    accessibilityLabel={t("model_row.remove_tooltip")}
                    tooltip={t("model_row.remove_tooltip")}
                    disabled={saving()}
                    onPress={() => props.page.removeCustomModelRow(row().key)}
                  />
                </View>
              </View>
            )}
          </Each>
        </View>
      </If>

      <If when={props.page.modelEditorError() != null}>
        <StatusLine text={props.page.modelEditorError() ?? ""} error={true} />
      </If>

      <View style={{ display: "flex", flexDirection: "row", justifyContent: "flex-end", gap: 8 }}>
        <SmeltButton
          variant="quiet"
          size="sm"
          disabled={saving()}
          label={t("common.cancel")}
          onPress={() => props.page.cancelCustomProviderEditor()}
        />
        <SmeltButton
          size="sm"
          disabled={saving()}
          label={t("provider.save_default")}
          onPress={() => props.page.saveCustomProvider(true)}
        />
        <SmeltButton
          variant="primary"
          size="sm"
          loading={saving()}
          disabled={saving()}
          label={t("common.save")}
          onPress={() => props.page.saveCustomProvider(false)}
        />
      </View>
    </View>
  );
}

function ModelDiscoverySection(props: {
  page: PageController;
  discovery: () => CustomProviderDraft["discovery"];
  adoptedIds: () => string[];
  manualRowCount: () => number;
}) {
  return (
    <>
      <If when={props.discovery() == null && props.manualRowCount() === 0}>
        <StatusLine text={t("discovery.auto")} />
      </If>
      <If when={props.discovery() == null && props.manualRowCount() > 0}>
        <StatusLine text={t("discovery.hint")} />
      </If>
      <If when={props.discovery()?.status === "loading"}>
        <StatusLine text={t("discovery.loading")} />
      </If>
      <If when={props.discovery()?.status === "failed"}>
        <StatusLine
          text={t("discovery.failed", { error: (props.discovery() as any)?.error ?? "" })}
          error={true}
        />
      </If>
      <If
        when={
          props.discovery()?.status === "ready" &&
          ((props.discovery() as any)?.models?.length ?? 0) === 0
        }
      >
        <StatusLine text={t("discovery.empty")} />
      </If>
      <If
        when={
          props.discovery()?.status === "ready" &&
          ((props.discovery() as any)?.models?.length ?? 0) > 0
        }
      >
        <View style={{ display: "flex", flexDirection: "column", gap: 6, width: "100%" }}>
          <StatusLine
            text={t("discovery.found", {
              count: (props.discovery() as any)?.models?.length ?? 0,
            })}
          />
          <View
            style={{
              display: "flex",
              flexDirection: "row",
              flexWrap: "wrap",
              alignItems: "center",
              gap: 6,
            }}
          >
            <Each of={(props.discovery() as any)?.models ?? []} key={(m: any) => m.id}>
              {(m: () => any) => {
                const taken = () => props.adoptedIds().includes(m().id);
                return (
                  <SmeltButton
                    size="sm"
                    selected={taken()}
                    disabled={taken()}
                    label={discoveredModelLabel(m())}
                    onPress={() => props.page.adoptDiscoveredModel(m())}
                  />
                );
              }}
            </Each>
          </View>
        </View>
      </If>
    </>
  );
}

// ---- 原生配置文件 ----

function NativeFilesSection(props: { page: PageController }) {
  const paths = computed(() => props.page.piConfig()?.paths ?? { settings: "", models: "", auth: "", mcp: "" });
  const isRemote = computed(() => props.page.machine() !== null);

  return (
    <SettingsSection>
      <SectionCell>
        <SettingsRow
          title={t("config.native_files")}
          description={
            isRemote() ? t("config.remote_file", { name: props.page.currentMachineName() }) : null
          }
        >
          <SmeltButton
            variant="quiet"
            size="sm"
            label="settings.json"
            tooltip={paths().settings || undefined}
            disabled={isRemote()}
            onPress={() => props.page.openConfigFile("settings")}
          />
          <SmeltButton
            variant="quiet"
            size="sm"
            label="models.json"
            tooltip={paths().models || undefined}
            disabled={isRemote()}
            onPress={() => props.page.openConfigFile("models")}
          />
          <SmeltButton
            variant="quiet"
            size="sm"
            label="auth.json"
            tooltip={paths().auth || undefined}
            disabled={isRemote()}
            onPress={() => props.page.openConfigFile("credentials")}
          />
          <SmeltButton
            variant="quiet"
            size="sm"
            icon="icons/refresh.svg"
            accessibilityLabel={t("config.reload_tooltip")}
            tooltip={t("config.reload_tooltip")}
            onPress={() => props.page.refreshSettingsAndAuth()}
          />
        </SettingsRow>
      </SectionCell>
    </SettingsSection>
  );
}
