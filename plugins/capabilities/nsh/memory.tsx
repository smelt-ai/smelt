// 设置「记忆」页：重排模型选择 + OpenAI 兼容向量检索服务配置与连通性测试。

import { computed, Text, View } from "nsh";
import { SmeltButton, SmeltMenuItem, SmeltMenuSeparator, t } from "smelt/sdk";
import {
  memoryKeyPlaceholder,
  memoryStatusLine,
  modelChoiceShortLabel,
  rerankModelLabel,
} from "../model.js";
import { currentTheme, type PageController } from "./state.ts";
import {
  DropdownMenuButton,
  MenuSub,
  PageIntro,
  SectionCell,
  SettingsPageShell,
  SettingsSection,
  StatusLine,
  TextField,
} from "./ui.tsx";

export function MemoryTab(props: { page: PageController }) {
  const look = currentTheme;
  const groups = computed(() => props.page.piConfig()?.modelChoiceGroups ?? []);
  const rerank = computed(() => props.page.memoryRerankModel());
  const saved = computed(() => props.page.memorySaved());
  const status = computed(() =>
    memoryStatusLine(saved(), props.page.memoryLoading(), t),
  );
  const keyPlaceholder = computed(() => memoryKeyPlaceholder(saved(), t));
  const testing = computed(() => props.page.memoryTesting());
  const saving = computed(() => props.page.memorySaving());
  const testResult = computed(() => props.page.memoryTestResult());

  return (
    <SettingsPageShell>
      <PageIntro text={t("memory.description")} />

      {/* 重排模型 */}
      <SettingsSection title={t("memory.rerank_title")}>
        <SectionCell>
          <View style={{ display: "flex", flexDirection: "column", width: "100%", gap: 8 }}>
            <View style={{ display: "flex", flexDirection: "row", alignItems: "center" }}>
              <DropdownMenuButton
                label={rerankModelLabel(rerank(), t)}
                align="start"
                minWidth={220}
              >
                <SmeltMenuItem
                  label={rerankModelLabel(null, t)}
                  checked={rerank() == null}
                  onSelect={() => props.page.setRerankModel(null)}
                />
                <If when={groups().length > 0}>
                  <SmeltMenuSeparator />
                </If>
                <Each of={groups()} key={(g: any) => g.provider}>
                  {(group: () => any) => {
                    const inGroup = () => rerank()?.provider === group().provider;
                    return (
                      <MenuSub label={group().provider_name} checked={inGroup()}>
                        <Each of={group().models} key={(c: any) => c.id}>
                          {(choice: () => any) => (
                            <SmeltMenuItem
                              label={modelChoiceShortLabel(choice())}
                              checked={inGroup() && rerank()?.id === choice().id}
                              onSelect={() =>
                                props.page.setRerankModel({
                                  provider: choice().provider,
                                  id: choice().id,
                                })
                              }
                            />
                          )}
                        </Each>
                      </MenuSub>
                    );
                  }}
                </Each>
              </DropdownMenuButton>
            </View>
            <StatusLine text={t("memory.rerank_description")} />
          </View>
        </SectionCell>
      </SettingsSection>

      {/* 向量检索服务 */}
      <SettingsSection title={t("memory.vector_title")}>
        <SectionCell>
          <View style={{ display: "flex", flexDirection: "column", width: "100%", gap: 12 }}>
            <StatusLine text={t("memory.form_description")} />
            <Text
              style={{
                fontSize: look().type.ui.size,
                lineHeight: look().type.ui.line_height,
                color: look().palette.text,
              }}
            >
              {status()}
            </Text>

            <TextField
              label={t("memory.base_url")}
              value={props.page.memoryBaseUrl()}
              placeholder={t("memory.base_url_placeholder")}
              disabled={saving()}
              onChange={(v) => {
                props.page.memoryBaseUrl.set(v);
                props.page.memoryTestResult.set(null);
              }}
              onSubmit={() => props.page.saveMemorySettings()}
            />

            <TextField
              label={t("memory.model")}
              value={props.page.memoryModel()}
              placeholder={t("memory.model_placeholder")}
              disabled={saving()}
              onChange={(v) => {
                props.page.memoryModel.set(v);
                props.page.memoryTestResult.set(null);
              }}
              onSubmit={() => props.page.saveMemorySettings()}
            />

            <TextField
              label={t("memory.api_key")}
              value={props.page.memoryApiKey()}
              placeholder={keyPlaceholder()}
              masked={true}
              disabled={saving()}
              onChange={(v) => {
                props.page.memoryApiKey.set(v);
                props.page.memoryTestResult.set(null);
              }}
              onSubmit={() => props.page.saveMemorySettings()}
            />

            <StatusLine text={t("memory.key_hint")} />

            <If when={props.page.memoryError() != null}>
              <StatusLine text={props.page.memoryError() ?? ""} error={true} />
            </If>

            <If when={testResult() != null}>
              <StatusLine
                text={
                  testResult()?.ok
                    ? testResult()?.text ?? ""
                    : t("memory.test_failed", { message: testResult()?.text ?? "" })
                }
                tone={testResult()?.ok ? "ok" : "bad"}
              />
            </If>

            <View
              style={{
                display: "flex",
                flexDirection: "row",
                alignItems: "center",
                flexWrap: "wrap",
                gap: 8,
              }}
            >
              <SmeltButton
                size="sm"
                label={testing() ? t("memory.testing") : t("memory.test")}
                tooltip={t("memory.test_tooltip")}
                loading={testing()}
                disabled={testing() || saving()}
                onPress={() => props.page.testMemoryEmbedding()}
              />
              <SmeltButton
                variant="primary"
                size="sm"
                label={t("common.save")}
                loading={saving()}
                disabled={saving()}
                onPress={() => props.page.saveMemorySettings()}
              />
              <If when={saved().apiKeySet}>
                <SmeltButton
                  variant="quiet"
                  size="sm"
                  label={t("memory.clear_key")}
                  disabled={saving()}
                  onPress={() => props.page.clearMemoryApiKey()}
                />
              </If>
              <SmeltButton
                variant="quiet"
                size="sm"
                label={t("memory.clear_all")}
                disabled={saving()}
                onPress={() => props.page.clearMemoryAll()}
              />
              <If when={props.page.memoryNotice() != null}>
                <StatusLine text={props.page.memoryNotice() ?? ""} />
              </If>
            </View>
          </View>
        </SectionCell>
      </SettingsSection>
    </SettingsPageShell>
  );
}
