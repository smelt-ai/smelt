//! 设置里的「模型」页：新对话默认用哪个模型、推理强度多大，接了哪些服务商，以及直接改
//! Pi 原生文件的入口。
//!
//! 默认模型的下拉只列现在真能用的：已登录的内置服务商报的模型（Pi 按凭据筛过，Copilot
//! 这类只列账号开了的）和自定义服务商的模型，找不到的走「手动填写…」。没选过就是「自动」，
//! 由 Pi 开会话时自己从已登录的服务商里挑。
//!
//! 服务商一行一个，只露当下该点的那个按钮（没登录的「登录…」），别的收进「⋯」。登录面板、
//! 编辑表单、删除确认就地展开在那一行下面。

use super::*;
use crate::models::PiConfigFileKind;
use api::model::pi_auth::{PiAuthProvider, PiAuthStatus, PiLoginMethod};
use api::model::pi_model_settings::{
    PI_THINKING_LEVELS, PiCustomProvider, PiModelChoiceGroup, PiModelRef, pi_thinking_level_label,
};
use smelt_plugin_api::ui::kit::TYPE;

pub(super) fn pi_model_page(
    entity: Entity<CapabilitiesPage>,
    page: &CapabilitiesPage,
    cx: &App,
) -> AnyElement {
    let page_body = SettingsPage::new("capabilities-pane:models");
    // 快照还没到（或读不到）时，下面几组都没有内容可画。
    if page.pi_library_config().is_none() {
        return page_body
            .child(SettingsSection::new().child(status_line(
                page.pi_library_pending_text(),
                page.pi_library_failed(),
            )))
            .into_any_element();
    }
    page_body
        .child(default_section(&entity, page))
        .child(provider_section(&entity, page, cx))
        .child(files_section(&entity, page, cx))
        .into_any_element()
}

/// 卡片里的一行字：读取中的说明，或就地报的错。
fn status_line(text: impl Into<SharedString>, error: bool) -> Div {
    div()
        .type_style(TYPE.text_small)
        .text_color(rgb(if error {
            ui_theme::bad()
        } else {
            ui_theme::muted()
        }))
        .child(text.into())
}

// ---- 默认模型 ----

/// 下拉里的一组：一个服务商和它能选的模型。
#[derive(Debug, PartialEq, Eq)]
struct ModelGroup {
    provider: String,
    name: String,
    /// (模型 ID, 显示名)
    models: Vec<(String, String)>,
}

/// 默认模型下拉的内容。
///
/// 自定义服务商的模型来自 `models.json`；内置服务商的来自凭据助手（只列已登录的）。助手还
/// 没答上来（或失败）时退回运行时快照里的目录，免得下拉空着。当前默认不在任何一组里（手填
/// 的、服务商没报的）也补进去，打开下拉看得到它被勾着。
fn model_groups(
    custom: &[PiCustomProvider],
    builtin: Option<&[PiAuthProvider]>,
    fallback: &[PiModelChoiceGroup],
    current: Option<&PiModelRef>,
) -> Vec<ModelGroup> {
    let is_custom = |id: &str| custom.iter().any(|provider| provider.id == id);
    let mut groups: Vec<ModelGroup> = custom
        .iter()
        .filter(|provider| !provider.models.is_empty())
        .map(|provider| ModelGroup {
            provider: provider.id.clone(),
            name: non_empty_or(&provider.display_name, &provider.id),
            models: provider
                .models
                .iter()
                .map(|model| (model.id.clone(), non_empty_or(&model.name, &model.id)))
                .collect(),
        })
        .collect();
    match builtin {
        Some(builtin) => groups.extend(
            builtin
                .iter()
                .filter(|provider| provider.status.configured() && !is_custom(&provider.id))
                .filter(|provider| !provider.models.is_empty())
                .map(|provider| ModelGroup {
                    provider: provider.id.clone(),
                    name: provider.name.clone(),
                    models: provider
                        .models
                        .iter()
                        .map(|model| (model.id.clone(), model.label().to_string()))
                        .collect(),
                }),
        ),
        None => groups.extend(
            fallback
                .iter()
                .filter(|group| !is_custom(&group.provider))
                .map(|group| ModelGroup {
                    provider: group.provider.clone(),
                    name: group.provider_name.clone(),
                    models: group
                        .models
                        .iter()
                        .map(|choice| (choice.id.clone(), choice.short_label().to_string()))
                        .collect(),
                }),
        ),
    }
    if let Some(current) = current {
        let listed = groups.iter().any(|group| {
            group.provider == current.provider
                && group.models.iter().any(|(id, _)| *id == current.model)
        });
        if !listed {
            let entry = (current.model.clone(), current.model.clone());
            match groups
                .iter_mut()
                .find(|group| group.provider == current.provider)
            {
                Some(group) => group.models.push(entry),
                None => groups.push(ModelGroup {
                    provider: current.provider.clone(),
                    name: current.provider.clone(),
                    models: vec![entry],
                }),
            }
        }
    }
    groups
}

fn non_empty_or(value: &str, fallback: &str) -> String {
    if value.trim().is_empty() {
        fallback.to_string()
    } else {
        value.to_string()
    }
}

/// 下拉按钮上显示的「服务商 / 模型」，用目录里的名字。
fn model_label(groups: &[ModelGroup], model: &PiModelRef) -> String {
    let group = groups.iter().find(|group| group.provider == model.provider);
    let provider = group.map_or(model.provider.as_str(), |group| group.name.as_str());
    let name = group
        .and_then(|group| group.models.iter().find(|(id, _)| *id == model.model))
        .map_or(model.model.as_str(), |(_, name)| name.as_str());
    format!("{provider} / {name}")
}

fn page_model_groups(page: &CapabilitiesPage) -> Vec<ModelGroup> {
    let Ok(settings) = page.pi_model_settings() else {
        return Vec::new();
    };
    let builtin = match &page.pi_auth_providers {
        Some(PiAuthProvidersState::Ready(list)) => Some(list.as_slice()),
        _ => None,
    };
    model_groups(
        &settings.custom_providers,
        builtin,
        &page.pi_model_choice_groups(),
        settings.default_model.as_ref(),
    )
}

/// 默认模型用的内置服务商没登录：新对话会起不来，就在那一行上说。
fn default_provider_signed_out(page: &CapabilitiesPage, model: &PiModelRef) -> bool {
    let custom = page.pi_model_settings().is_ok_and(|settings| {
        settings
            .custom_providers
            .iter()
            .any(|provider| provider.id == model.provider)
    });
    let Some(PiAuthProvidersState::Ready(builtin)) = &page.pi_auth_providers else {
        return false;
    };
    !custom
        && builtin
            .iter()
            .any(|provider| provider.id == model.provider && !provider.status.configured())
}

fn default_section(entity: &Entity<CapabilitiesPage>, page: &CapabilitiesPage) -> SettingsSection {
    let section = SettingsSection::new();
    let settings = match page.pi_model_settings() {
        Ok(settings) => settings,
        Err(error) => {
            return section.child(status_line(
                t!("default_model.read_failed", error = error),
                true,
            ));
        }
    };
    let editing = page.pi_model_editor.is_some();
    let current = settings.default_model.clone();
    let groups = page_model_groups(page);

    let label = match &current {
        Some(model) => model_label(&groups, model),
        None => t!("default_model.auto").into_owned(),
    };
    let signed_out = current
        .as_ref()
        .is_some_and(|model| default_provider_signed_out(page, model));
    let mut model_row = SettingsRow::new(t!("default_model.title"));
    if current.is_none() {
        model_row = model_row.description(t!("default_model.auto_description"));
    }
    let model_row = model_row
        .children(signed_out.then(|| {
            Badge::new()
                .need()
                .child(t!("default_model.signed_out").into_owned())
        }))
        .child(default_model_picker(entity, label, current, editing));

    let thinking = settings.thinking_level.clone();
    let thinking_label = thinking
        .as_deref()
        .map(pi_thinking_level_label)
        .unwrap_or_else(|| t!("default_model.thinking_unset").into_owned());
    let thinking_entity = entity.clone();
    let thinking_trigger = Button::new("pi-thinking-level")
        .small()
        .dropdown_caret(true)
        .label(thinking_label);
    let thinking_row = SettingsRow::new(t!("default_model.thinking")).child(MenuTrigger::dropdown(
        "pi-thinking-level-menu",
        thinking_trigger,
        move |_, _| {
            PI_THINKING_LEVELS.iter().fold(Menu::new(), |menu, level| {
                let entity = thinking_entity.clone();
                menu.item(
                    MenuItem::new(pi_thinking_level_label(level))
                        .checked(thinking.as_deref() == Some(*level))
                        .on_click(move |_, _, cx| {
                            entity.update(cx, |page, cx| {
                                page.set_pi_default_thinking_level(level.to_string(), cx);
                            });
                        }),
                )
            })
        },
    ));

    section
        .child(model_row)
        .children(
            page.pi_model_editor.as_ref().map(|editor| {
                manual_model_form(entity, editor, page.pi_model_editor_error.clone())
            }),
        )
        .children(
            page.pi_default_error
                .clone()
                .map(|error| status_line(t!("common.action_failed", error = error), true)),
        )
        .child(thinking_row)
}

/// 默认模型的下拉：自动、按服务商分组的模型、手动填写。打开时才取目录，拿的是最新的。
fn default_model_picker(
    entity: &Entity<CapabilitiesPage>,
    label: String,
    current: Option<PiModelRef>,
    editing: bool,
) -> impl IntoElement {
    let entity = entity.clone();
    let trigger = Button::new("pi-default-model")
        .small()
        .dropdown_caret(true)
        .label(label)
        .disabled(editing);
    MenuTrigger::dropdown("pi-default-model-menu", trigger, move |_, cx| {
        let groups = page_model_groups(entity.read(cx));
        let auto_entity = entity.clone();
        let mut menu = Menu::new().item(
            MenuItem::new(t!("default_model.auto"))
                .checked(current.is_none())
                .on_click(move |_, _, cx| {
                    auto_entity.update(cx, |page, cx| page.clear_pi_default_model(cx));
                }),
        );
        menu = menu.separator();
        if groups.is_empty() {
            menu = menu.item(MenuItem::new(t!("default_model.none")).disabled(true));
        }
        for group in groups {
            let in_group = current
                .as_ref()
                .is_some_and(|model| model.provider == group.provider);
            let mut sub = Menu::new();
            for (id, name) in group.models {
                let checked = in_group && current.as_ref().is_some_and(|model| model.model == id);
                let item_entity = entity.clone();
                let provider = group.provider.clone();
                sub = sub.item(
                    MenuItem::new(name)
                        .checked(checked)
                        .on_click(move |_, _, cx| {
                            let (provider, model) = (provider.clone(), id.clone());
                            item_entity.update(cx, |page, cx| {
                                page.set_pi_default_model(provider, model, cx);
                            });
                        }),
                );
            }
            menu = menu.item(MenuItem::new(group.name).checked(in_group).submenu(sub));
        }
        let manual_entity = entity.clone();
        menu.separator()
            .item(
                MenuItem::new(t!("default_model.manual_menu")).on_click(move |_, window, cx| {
                    manual_entity.update(cx, |page, cx| page.edit_pi_model_settings(window, cx));
                }),
            )
    })
    .disabled(editing)
}

// ---- 服务商 ----

/// 一行服务商：内置的（Pi 认识、走登录）或自定义的（`models.json` 里的网关）。
#[derive(Debug, PartialEq, Eq)]
enum ProviderEntry<'a> {
    Builtin(&'a PiAuthProvider),
    Custom(&'a PiCustomProvider),
}

impl ProviderEntry<'_> {
    fn id(&self) -> &str {
        match self {
            Self::Builtin(provider) => &provider.id,
            Self::Custom(provider) => &provider.id,
        }
    }

    fn configured(&self) -> bool {
        match self {
            Self::Builtin(provider) => provider.status.configured(),
            // 自定义服务商是用户自己加的，不管有没有 key 都算在用。
            Self::Custom(_) => true,
        }
    }
}

/// 合并内置与自定义服务商，返回要显示的行和折叠掉的内置服务商数。
///
/// 排序：默认模型用的那个 → 已配置 / 自定义 → 其余，同一档内保持原有顺序。自定义服务商的
/// id 若也在内置列表里（Pi 会把 `models.json` 里的一起报上来），只留自定义那一行，编辑入口在
/// 那边。`show_all` 为假时只列能订阅登录或已配置的内置项：Pi 内置四十来个，常动的只有这几个。
fn merge_providers<'a>(
    builtin: &'a [PiAuthProvider],
    custom: &'a [PiCustomProvider],
    default_provider: &str,
    show_all: bool,
) -> (Vec<ProviderEntry<'a>>, usize) {
    let mut hidden = 0;
    let mut entries: Vec<ProviderEntry<'a>> = custom.iter().map(ProviderEntry::Custom).collect();
    for provider in builtin {
        if custom.iter().any(|custom| custom.id == provider.id) {
            continue;
        }
        let visible = show_all
            || provider.supports_oauth
            || provider.status.configured()
            || provider.id == default_provider;
        if visible {
            entries.push(ProviderEntry::Builtin(provider));
        } else {
            hidden += 1;
        }
    }
    entries.sort_by_key(|entry| {
        if entry.id() == default_provider {
            0
        } else if entry.configured() {
            1
        } else {
            2
        }
    });
    (entries, hidden)
}

fn provider_section(
    entity: &Entity<CapabilitiesPage>,
    page: &CapabilitiesPage,
    cx: &App,
) -> SettingsSection {
    let mut section = SettingsSection::new().title(t!("providers.title"));
    if let Some(error) = &page.pi_auth_error {
        section = section.child(status_line(t!("common.action_failed", error = error), true));
    }
    let settings = page.pi_model_settings().ok();
    let custom: &[PiCustomProvider] = settings
        .map(|settings| settings.custom_providers.as_slice())
        .unwrap_or_default();
    let default_provider = settings
        .and_then(|settings| settings.default_model.as_ref())
        .map(|model| model.provider.as_str())
        .unwrap_or_default();
    let builtin: &[PiAuthProvider] = match &page.pi_auth_providers {
        Some(PiAuthProvidersState::Ready(list)) => list,
        _ => &[],
    };
    let auto_ids = page
        .pi_library_config()
        .map(|config| config.auto_provider_ids.as_slice())
        .unwrap_or_default();
    let (entries, hidden) =
        merge_providers(builtin, custom, default_provider, page.pi_auth_show_all);
    // MCP 服务器的登录画在 MCP 页签。
    let login = page
        .pi_login
        .as_ref()
        .filter(|view| view.provider_id().is_some());
    let login_running = login.is_some_and(|view| view.error.is_none());
    let editor = page.pi_custom_provider_editor.as_ref();
    let busy = RowBusy {
        login: login_running,
        editing: editor.is_some(),
        confirming: page.pi_provider_confirm.is_some(),
    };

    let mut rendered_login = false;
    for entry in &entries {
        let id = entry.id();
        section = match entry {
            ProviderEntry::Builtin(provider) => section.child(builtin_row(entity, provider, busy)),
            ProviderEntry::Custom(provider) => section.child(custom_row(
                entity,
                provider,
                auto_ids.iter().any(|auto| auto == id),
                busy,
            )),
        };
        if let Some(confirm) = page
            .pi_provider_confirm
            .as_ref()
            .filter(|confirm| confirm.provider_id() == id)
        {
            let name = match entry {
                ProviderEntry::Builtin(provider) => provider.name.clone(),
                ProviderEntry::Custom(provider) => non_empty_or(&provider.display_name, id),
            };
            section = section.child(confirm_line(entity, confirm, &name));
        }
        if let Some(view) = login.filter(|view| view.provider_id() == Some(id)) {
            rendered_login = true;
            section = section.child(super::auth::login_panel(entity.clone(), view, cx));
        }
        if let Some(editor) = editor.filter(|editor| editor.previous_id.as_deref() == Some(id)) {
            section = section.child(custom_provider_form(
                entity,
                editor,
                page.pi_model_editor_error.clone(),
                cx,
            ));
        }
    }
    // 登录的那个服务商可能被折叠了（刚切回只看常用），面板不能跟着消失。
    if let Some(view) = login.filter(|_| !rendered_login) {
        section = section.child(super::auth::login_panel(entity.clone(), view, cx));
    }
    if let Some(editor) = editor.filter(|editor| editor.previous_id.is_none()) {
        section = section.child(custom_provider_form(
            entity,
            editor,
            page.pi_model_editor_error.clone(),
            cx,
        ));
    }
    section.child(provider_footer(entity, page, hidden, busy))
}

/// 服务商行上的按钮什么时候置灰：同时只跑一个登录、开一张表单、等一个确认。
#[derive(Clone, Copy)]
struct RowBusy {
    login: bool,
    editing: bool,
    confirming: bool,
}

impl RowBusy {
    fn any(self) -> bool {
        self.login || self.editing || self.confirming
    }
}

/// 「⋯」按钮：一行里不常用的动作。
fn more_menu(id: &str, disabled: bool, build: impl Fn() -> Menu + 'static) -> impl IntoElement {
    let trigger = Button::new(SharedString::from(format!("pi-provider-more-{id}")))
        .quiet()
        .small()
        .icon(IconName::Ellipsis)
        .accessibility_label(t!("auth.more_actions"))
        .disabled(disabled);
    MenuTrigger::dropdown(
        SharedString::from(format!("pi-provider-more-menu-{id}")),
        trigger,
        move |_, _| build(),
    )
    .disabled(disabled)
}

fn builtin_row(
    entity: &Entity<CapabilitiesPage>,
    provider: &PiAuthProvider,
    busy: RowBusy,
) -> SettingsRow {
    let configured = provider.status.configured();
    // 凭据从哪来要说清楚：环境变量里的 key 和存在 auth.json 里的是两回事，注销只动得了后者。
    let note = match (provider.status, &provider.source) {
        (PiAuthStatus::Oauth, _) => t!("auth.status.oauth").into_owned(),
        (PiAuthStatus::ApiKey, Some(source)) => {
            t!("auth.status.api_key_from", source = source).into_owned()
        }
        (PiAuthStatus::ApiKey, None) => t!("auth.status.api_key").into_owned(),
        (PiAuthStatus::None, _) if provider.subscription => {
            t!("auth.status.none_subscription").into_owned()
        }
        (PiAuthStatus::None, _) => t!("auth.status.none").into_owned(),
    };

    let login = |method: PiLoginMethod, ask_all: bool| {
        let entity = entity.clone();
        let id = provider.id.clone();
        let name = provider.name.clone();
        move |window: &mut Window, cx: &mut App| {
            let (id, name) = (id.clone(), name.clone());
            entity.update(cx, |page, cx| {
                page.start_pi_login(id, name, method, ask_all, window, cx);
            });
        }
    };

    // 没配好的才有主按钮；配好了的动作都收进「⋯」。
    let primary = if configured {
        None
    } else if provider.supports_oauth {
        let start = login(PiLoginMethod::OAuth, false);
        Some(
            Button::new(SharedString::from(format!("pi-auth-oauth-{}", provider.id)))
                .small()
                .label(t!("auth.sign_in"))
                .tooltip(t!("auth.sign_in_tooltip"))
                .disabled(busy.any())
                .on_click(move |_, window, cx| start(window, cx)),
        )
    } else if provider.supports_api_key {
        let start = login(PiLoginMethod::ApiKey, true);
        Some(
            Button::new(SharedString::from(format!("pi-auth-key-{}", provider.id)))
                .small()
                .label(t!("auth.enter_api_key"))
                .disabled(busy.any())
                .on_click(move |_, window, cx| start(window, cx)),
        )
    } else {
        None
    };

    // 主按钮已经占掉的那个不再重复。
    let reauth = (configured && provider.supports_oauth)
        .then(|| std::rc::Rc::new(login(PiLoginMethod::OAuth, false)));
    // 自动跳过的步骤总有人要自己答：GitHub 企业版域名、换一种登录方式。
    let step_by_step = provider
        .supports_oauth
        .then(|| std::rc::Rc::new(login(PiLoginMethod::OAuth, true)));
    let api_key = (provider.supports_api_key && (configured || provider.supports_oauth))
        .then(|| std::rc::Rc::new(login(PiLoginMethod::ApiKey, true)));
    let api_key_label = if provider.status == PiAuthStatus::ApiKey {
        t!("auth.change_api_key")
    } else {
        t!("auth.use_api_key")
    };
    let sign_out = configured.then(|| {
        let entity = entity.clone();
        let id = provider.id.clone();
        std::rc::Rc::new(move |cx: &mut App| {
            let confirm = PiProviderConfirm::SignOut(id.clone());
            entity.update(cx, |page, cx| page.request_pi_provider_confirm(confirm, cx));
        })
    });
    let has_more =
        reauth.is_some() || step_by_step.is_some() || api_key.is_some() || sign_out.is_some();
    let more = has_more.then(|| {
        more_menu(&provider.id, busy.any(), move || {
            let mut menu = Menu::new();
            if let Some(start) = reauth.clone() {
                menu = menu.item(
                    MenuItem::new(t!("auth.sign_in_again"))
                        .on_click(move |_, window, cx| start(window, cx)),
                );
            }
            if let Some(start) = step_by_step.clone() {
                menu = menu.item(
                    MenuItem::new(t!("auth.sign_in_step_by_step"))
                        .on_click(move |_, window, cx| start(window, cx)),
                );
            }
            if let Some(start) = api_key.clone() {
                menu = menu.item(
                    MenuItem::new(api_key_label.clone())
                        .on_click(move |_, window, cx| start(window, cx)),
                );
            }
            if let Some(sign_out) = sign_out.clone() {
                menu = menu.separator().item(
                    MenuItem::new(t!("auth.sign_out"))
                        .danger()
                        .on_click(move |_, _, cx| sign_out(cx)),
                );
            }
            menu
        })
    });

    SettingsRow::new(provider.name.clone())
        .note(note)
        .children(primary)
        .children(more)
}

fn custom_row(
    entity: &Entity<CapabilitiesPage>,
    provider: &PiCustomProvider,
    auto: bool,
    busy: RowBusy,
) -> SettingsRow {
    let mut note = t!("provider.model_count", count = provider.models.len()).into_owned();
    if auto {
        note = format!("{note} · {}", t!("provider.auto_synced"));
    }
    if !provider.credential_configured {
        note = format!("{note} · {}", t!("provider.no_key"));
    }

    let edit_entity = entity.clone();
    let edit_id = provider.id.clone();
    let edit = Button::new(SharedString::from(format!(
        "pi-edit-provider-{}",
        provider.id
    )))
    .quiet()
    .small()
    .label(t!("provider.edit"))
    .disabled(busy.any())
    .on_click(move |_, window, cx| {
        let id = edit_id.clone();
        edit_entity.update(cx, |page, cx| {
            page.edit_pi_custom_provider(Some(id), window, cx);
        });
    });
    let remove_entity = entity.clone();
    let remove_id = provider.id.clone();
    let more =
        more_menu(&provider.id, busy.any(), move || {
            let entity = remove_entity.clone();
            let id = remove_id.clone();
            Menu::new().item(MenuItem::new(t!("provider.remove")).danger().on_click(
                move |_, _, cx| {
                    let confirm = PiProviderConfirm::RemoveCustom(id.clone());
                    entity.update(cx, |page, cx| page.request_pi_provider_confirm(confirm, cx));
                },
            ))
        });

    SettingsRow::new(non_empty_or(&provider.display_name, &provider.id))
        .note(note)
        .child(edit)
        .child(more)
}

/// 删除自定义服务商、注销登录前问一句，说清楚丢掉的是什么。
fn confirm_line(
    entity: &Entity<CapabilitiesPage>,
    confirm: &PiProviderConfirm,
    name: &str,
) -> SettingsRow {
    let (question, action) = match confirm {
        PiProviderConfirm::RemoveCustom(_) => (
            t!("provider.remove_question", name = name),
            t!("provider.remove_confirm"),
        ),
        PiProviderConfirm::SignOut(_) => (
            t!("auth.sign_out_question", name = name),
            t!("auth.sign_out"),
        ),
    };
    let cancel_entity = entity.clone();
    let confirm_entity = entity.clone();
    let confirmed = confirm.clone();
    SettingsRow::new(question)
        .child(
            Button::new("pi-provider-confirm-cancel")
                .quiet()
                .small()
                .label(t!("common.cancel"))
                .on_click(move |_, _, cx| {
                    cancel_entity.update(cx, |page, cx| page.cancel_pi_provider_confirm(cx));
                }),
        )
        .child(
            Button::new("pi-provider-confirm")
                .danger()
                .small()
                .label(action)
                .on_click(move |_, _, cx| {
                    let confirmed = confirmed.clone();
                    confirm_entity.update(cx, |page, cx| {
                        page.confirm_pi_provider_action(confirmed, cx);
                    });
                }),
        )
}

/// 表尾：读取状态、「显示全部内置服务商」，和添加自定义服务商。
fn provider_footer(
    entity: &Entity<CapabilitiesPage>,
    page: &CapabilitiesPage,
    hidden: usize,
    busy: RowBusy,
) -> Div {
    let left: AnyElement = match &page.pi_auth_providers {
        // 进页签时会自动读；这里只是读之前的一帧。
        None | Some(PiAuthProvidersState::Loading) => {
            status_line(t!("providers.loading"), false).into_any_element()
        }
        Some(PiAuthProvidersState::Failed(error)) => {
            let retry_entity = entity.clone();
            div()
                .flex()
                .items_center()
                .min_w_0()
                .gap_2()
                .child(status_line(t!("providers.failed", error = error), true).min_w_0())
                .child(
                    Button::new("pi-auth-retry")
                        .quiet()
                        .small()
                        .label(t!("common.retry"))
                        .on_click(move |_, _, cx| {
                            retry_entity.update(cx, |page, cx| page.refresh_pi_auth_providers(cx));
                        }),
                )
                .into_any_element()
        }
        Some(PiAuthProvidersState::Ready(_)) => {
            let show_all = page.pi_auth_show_all;
            let toggle_entity = entity.clone();
            div()
                .children((show_all || hidden > 0).then(|| {
                    Button::new("pi-auth-toggle-all")
                        .quiet()
                        .small()
                        .label(if show_all {
                            t!("providers.show_common").into_owned()
                        } else {
                            t!("providers.show_all", hidden = hidden).into_owned()
                        })
                        .on_click(move |_, _, cx| {
                            toggle_entity.update(cx, |page, cx| page.toggle_pi_auth_show_all(cx));
                        })
                }))
                .into_any_element()
        }
    };
    let add_entity = entity.clone();
    div()
        .flex()
        .items_center()
        .justify_between()
        .w_full()
        .gap_3()
        .child(div().flex_1().min_w_0().child(left))
        .child(
            Button::new("pi-add-custom-provider")
                .quiet()
                .small()
                .icon(IconName::Plus)
                .label(t!("provider.add"))
                .tooltip(t!("provider.add_tooltip"))
                .disabled(busy.any())
                .on_click(move |_, window, cx| {
                    add_entity.update(cx, |page, cx| {
                        page.edit_pi_custom_provider(None, window, cx);
                    });
                }),
        )
}

// ---- 原生配置文件 ----

/// 直接改 Pi 原生文件的逃生口。文件在运行时那台机器上，只有本机的打得开。
fn files_section(
    entity: &Entity<CapabilitiesPage>,
    page: &CapabilitiesPage,
    cx: &App,
) -> SettingsSection {
    let paths = page.pi_paths();
    let remote = match &page.machine {
        RuntimeKey::Local => None,
        RuntimeKey::Remote(_) => Some(
            api::host(cx)
                .machines(cx)
                .into_iter()
                .find(|machine| machine.key == page.machine)
                .map(|machine| machine.name)
                .unwrap_or_default(),
        ),
    };
    let file_button = |id: &'static str, label: &'static str, kind: PiConfigFileKind, path| {
        let entity = entity.clone();
        Button::new(id)
            .quiet()
            .small()
            .label(label)
            .tooltip(SharedString::from(path))
            .disabled(remote.is_some())
            .on_click(move |_, window, cx| {
                entity.update(cx, |page, cx| page.open_pi_config(kind, window, cx));
            })
    };
    let refresh_entity = entity.clone();
    let mut row = SettingsRow::new(t!("config.native_files"));
    if let Some(name) = &remote {
        row = row.description(t!("config.remote_file", name = name));
    }
    let row = row
        .child(file_button(
            "pi-edit-settings-file",
            "settings.json",
            PiConfigFileKind::Settings,
            paths.settings,
        ))
        .child(file_button(
            "pi-edit-models-file",
            "models.json",
            PiConfigFileKind::Models,
            paths.models,
        ))
        .child(file_button(
            "pi-edit-credentials-file",
            "auth.json",
            PiConfigFileKind::Credentials,
            paths.auth,
        ))
        .child(
            Button::new("pi-refresh-settings")
                .quiet()
                .small()
                .icon(IconName::Refresh)
                .accessibility_label(t!("config.reload_tooltip"))
                .tooltip(t!("config.reload_tooltip"))
                .on_click(move |_, _, cx| {
                    refresh_entity.update(cx, |page, cx| {
                        page.refresh_pi_settings(cx);
                        page.refresh_pi_auth_providers(cx);
                    });
                }),
        );
    SettingsSection::new().child(row)
}

// ---- 表单 ----

fn field(label: impl Into<SharedString>, input: &Entity<InputState>) -> Div {
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .gap_1()
        .child(
            div()
                .type_style(TYPE.text_xsmall_medium)
                .text_color(rgb(ui_theme::text()))
                .child(label.into()),
        )
        .child(TextInput::new(input))
}

fn field_pair(left: impl IntoElement, right: impl IntoElement) -> Div {
    div()
        .flex()
        .items_start()
        .w_full()
        .gap_3()
        .child(left)
        .child(right)
}

fn form_actions() -> Div {
    div().flex().items_center().justify_end().w_full().gap_2()
}

/// 「手动填写…」：直接填服务商 / 模型 ID / 地址 / key，给下拉里找不到的模型用。
fn manual_model_form(
    entity: &Entity<CapabilitiesPage>,
    editor: &PiModelEditor,
    error: Option<String>,
) -> Div {
    let save_entity = entity.clone();
    let cancel_entity = entity.clone();
    div()
        .flex()
        .flex_col()
        .w_full()
        .gap_3()
        .py_1()
        .child(field_pair(
            field(t!("model_editor.provider"), &editor.provider),
            field(t!("model_editor.model"), &editor.model),
        ))
        .child(field_pair(
            field(t!("model_editor.base_url"), &editor.base_url),
            field(t!("model_editor.api_key"), &editor.api_key),
        ))
        .children(error.map(|error| status_line(error, true)))
        .child(
            form_actions()
                .child(
                    Button::new("pi-cancel-model-settings")
                        .quiet()
                        .small()
                        .label(t!("common.cancel"))
                        .on_click(move |_, _, cx| {
                            cancel_entity.update(cx, |page, cx| page.cancel_pi_model_edit(cx));
                        }),
                )
                .child(
                    Button::new("pi-save-model-settings")
                        .primary()
                        .small()
                        .label(t!("common.save"))
                        .on_click(move |_, window, cx| {
                            save_entity.update(cx, |page, cx| {
                                page.save_pi_model_settings(window, cx);
                            });
                        }),
                ),
        )
}

fn custom_provider_form(
    entity: &Entity<CapabilitiesPage>,
    editor: &PiCustomProviderEditor,
    error: Option<String>,
    cx: &App,
) -> Div {
    let muted: Hsla = rgb(ui_theme::muted()).into();
    let danger: Hsla = rgb(ui_theme::bad()).into();
    let discovery = editor.discovery.clone();
    let discovering = matches!(discovery, Some(ModelDiscoveryState::Loading));
    let adopted = editor
        .models
        .iter()
        .map(|row| row.id.read(cx).value().trim().to_string())
        .filter(|id| !id.is_empty())
        .collect::<Vec<_>>();

    let title = match &editor.previous_id {
        Some(id) => t!("provider.edit_title", id = id),
        None => t!("provider.add_title"),
    };

    let api_entity = entity.clone();
    let api_picker = div()
        .flex()
        .flex_col()
        .flex_none()
        .gap_1()
        .child(
            div()
                .type_style(TYPE.text_xsmall_medium)
                .text_color(rgb(ui_theme::text()))
                .child(t!("provider.api").into_owned()),
        )
        .child(MenuTrigger::dropdown(
            "pi-custom-provider-api-menu",
            Button::new("pi-custom-provider-api")
                .small()
                .dropdown_caret(true)
                .label(editor.api.clone()),
            move |_, _| {
                api::model::provider_api::PROVIDER_API_PROTOCOLS
                    .iter()
                    .fold(Menu::new(), |menu, protocol| {
                        let entity = api_entity.clone();
                        menu.item(MenuItem::new(*protocol).on_click(move |_, _, cx| {
                            entity.update(cx, |page, cx| page.set_pi_custom_api(protocol, cx));
                        }))
                    })
            },
        ));

    let discover_entity = entity.clone();
    let add_model_entity = entity.clone();
    let models_header = div()
        .flex()
        .items_center()
        .justify_between()
        .w_full()
        .child(
            div()
                .type_style(TYPE.ui_medium)
                .text_color(rgb(ui_theme::text()))
                .child(t!("provider.models").into_owned()),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(
                    Button::new("pi-discover-custom-models")
                        .quiet()
                        .small()
                        .label(t!("provider.fetch_models"))
                        .loading(discovering)
                        .disabled(discovering)
                        .on_click(move |_, _, cx| {
                            discover_entity
                                .update(cx, |page, cx| page.discover_pi_custom_models(cx));
                        }),
                )
                .child(
                    Button::new("pi-add-custom-model")
                        .quiet()
                        .small()
                        .icon(IconName::Plus)
                        .label(t!("provider.add_model"))
                        .on_click(move |_, window, cx| {
                            add_model_entity.update(cx, |page, cx| {
                                page.add_pi_custom_model(window, cx);
                            });
                        }),
                ),
        );

    let cancel_entity = entity.clone();
    let save_entity = entity.clone();
    let save_default_entity = entity.clone();
    div()
        .flex()
        .flex_col()
        .w_full()
        .gap_3()
        .py_1()
        .child(
            div()
                .type_style(TYPE.ui_medium)
                .text_color(rgb(ui_theme::text()))
                .child(title.into_owned()),
        )
        .child(field_pair(
            field(t!("provider.id"), &editor.id),
            field(t!("provider.name"), &editor.display_name),
        ))
        .child(field_pair(
            field(t!("provider.base_url"), &editor.base_url),
            api_picker,
        ))
        .child(field(t!("provider.api_key"), &editor.api_key))
        .child(models_header)
        .child(render_model_discovery_section(
            "pi",
            discovery,
            adopted,
            editor.models.len(),
            entity.clone(),
            CapabilitiesPage::adopt_pi_discovered_model,
            muted,
            danger,
        ))
        .children(model_table(entity, &editor.models))
        .children(error.map(|error| status_line(error, true)))
        .child(
            form_actions()
                .child(
                    Button::new("pi-cancel-custom-provider")
                        .quiet()
                        .small()
                        .label(t!("common.cancel"))
                        .on_click(move |_, _, cx| {
                            cancel_entity.update(cx, |page, cx| page.cancel_pi_custom_provider(cx));
                        }),
                )
                .child(
                    Button::new("pi-save-custom-provider-default")
                        .small()
                        .label(t!("provider.save_default"))
                        .on_click(move |_, window, cx| {
                            save_default_entity.update(cx, |page, cx| {
                                page.save_pi_custom_provider(true, window, cx);
                            });
                        }),
                )
                .child(
                    Button::new("pi-save-custom-provider")
                        .primary()
                        .small()
                        .label(t!("common.save"))
                        .on_click(move |_, window, cx| {
                            save_entity.update(cx, |page, cx| {
                                page.save_pi_custom_provider(false, window, cx);
                            });
                        }),
                ),
        )
}

/// 自定义服务商的模型：一行一个，列名只在表头写一次。没有行时不画。
fn model_table(entity: &Entity<CapabilitiesPage>, rows: &[PiCustomModelEditor]) -> Option<Div> {
    if rows.is_empty() {
        return None;
    }
    let header = |text: std::borrow::Cow<'static, str>, grow: f32| {
        div()
            .flex_grow(grow)
            .flex_basis(px(0.))
            .min_w_0()
            .type_style(TYPE.text_xsmall)
            .text_color(rgb(ui_theme::muted()))
            .child(text.into_owned())
    };
    let cell = |input: &Entity<InputState>, grow: f32| {
        div()
            .flex_grow(grow)
            .flex_basis(px(0.))
            .min_w_0()
            .child(TextInput::new(input))
    };
    // 四列按 3:3:2:2 分宽；删除按钮那一列的宽度，表头留同样的空。
    let trash_slot = || div().flex_none().w(rems(1.75));
    let mut table = div().flex().flex_col().w_full().gap_2().child(
        div()
            .flex()
            .items_center()
            .w_full()
            .gap_2()
            .child(header(t!("model_row.id"), 3.))
            .child(header(t!("model_row.name"), 3.))
            .child(header(t!("model_row.context"), 2.))
            .child(header(t!("model_row.max_tokens"), 2.))
            .child(trash_slot()),
    );
    for (index, model) in rows.iter().enumerate() {
        let remove_entity = entity.clone();
        table = table.child(
            div()
                .id(("pi-custom-model", model.id.entity_id()))
                .flex()
                .items_center()
                .w_full()
                .gap_2()
                .child(cell(&model.id, 3.))
                .child(cell(&model.name, 3.))
                .child(cell(&model.context_window, 2.))
                .child(cell(&model.max_tokens, 2.))
                .child(
                    trash_slot().child(
                        Button::new("pi-remove-custom-model")
                            .quiet()
                            .small()
                            .icon(IconName::Trash)
                            .accessibility_label(t!("model_row.remove_tooltip"))
                            .tooltip(t!("model_row.remove_tooltip"))
                            .on_click(move |_, _, cx| {
                                remove_entity.update(cx, |page, cx| {
                                    page.remove_pi_custom_model(index, cx);
                                });
                            }),
                    ),
                ),
        );
    }
    Some(table)
}

#[cfg(test)]
mod tests {
    // 不用 `super::*`：那条 glob 链会带进 `gpui::*` 里的 `test` 属性宏，把这里的
    // `#[test]` 换成 gpui 的版本，展开时撞上递归上限。
    use super::{ModelGroup, ProviderEntry, merge_providers, model_groups, model_label};
    use smelt_plugin_api::model::pi_auth::{PiAuthProvider, PiAuthStatus};
    use smelt_plugin_api::model::pi_model_settings::{
        PiCustomModel, PiCustomProvider, PiModelChoice, PiModelChoiceGroup, PiModelRef,
    };
    use smelt_plugin_api::model::provider_api::DiscoveredModel;

    fn builtin(id: &str, oauth: bool, status: PiAuthStatus) -> PiAuthProvider {
        PiAuthProvider {
            id: id.into(),
            name: id.into(),
            supports_oauth: oauth,
            status,
            ..Default::default()
        }
    }

    fn custom(id: &str) -> PiCustomProvider {
        PiCustomProvider {
            id: id.into(),
            ..Default::default()
        }
    }

    fn ids(entries: &[ProviderEntry<'_>]) -> Vec<String> {
        entries.iter().map(|entry| entry.id().to_string()).collect()
    }

    fn model_ref(provider: &str, model: &str) -> PiModelRef {
        PiModelRef {
            provider: provider.into(),
            model: model.into(),
        }
    }

    fn discovered(id: &str) -> DiscoveredModel {
        DiscoveredModel {
            id: id.into(),
            name: None,
            context_window: None,
            max_tokens: None,
        }
    }

    #[test]
    fn merged_providers_put_default_then_configured_first() {
        let builtin = vec![
            builtin("copilot", true, PiAuthStatus::None),
            builtin("openai", false, PiAuthStatus::ApiKey),
            builtin("anthropic", true, PiAuthStatus::Oauth),
        ];
        let custom = vec![custom("gateway")];
        let (entries, hidden) = merge_providers(&builtin, &custom, "anthropic", false);
        assert_eq!(ids(&entries), ["anthropic", "gateway", "openai", "copilot"]);
        assert_eq!(hidden, 0);
    }

    #[test]
    fn rarely_used_builtins_fold_until_show_all() {
        let builtin = vec![
            builtin("groq", false, PiAuthStatus::None),
            builtin("copilot", true, PiAuthStatus::None),
        ];
        let (entries, hidden) = merge_providers(&builtin, &[], "", false);
        assert_eq!(ids(&entries), ["copilot"]);
        assert_eq!(hidden, 1);

        let (entries, hidden) = merge_providers(&builtin, &[], "", true);
        assert_eq!(ids(&entries), ["groq", "copilot"]);
        assert_eq!(hidden, 0);
    }

    #[test]
    fn default_builtin_stays_visible_even_when_folded() {
        let builtin = vec![builtin("groq", false, PiAuthStatus::None)];
        let (entries, hidden) = merge_providers(&builtin, &[], "groq", false);
        assert_eq!(
            ids(&entries),
            ["groq"],
            "默认模型用的服务商没登录时，得能在列表里找到它去登录"
        );
        assert_eq!(hidden, 0);
    }

    #[test]
    fn custom_provider_shadows_builtin_with_same_id() {
        let builtin = vec![builtin("gateway", false, PiAuthStatus::ApiKey)];
        let custom = vec![custom("gateway")];
        let (entries, _) = merge_providers(&builtin, &custom, "", false);
        assert_eq!(entries, vec![ProviderEntry::Custom(&custom[0])]);
    }

    /// 下拉只列能用的：自定义的全列，内置的只列登录了的；Anthropic 这类静态目录的也在。
    #[test]
    fn model_groups_list_custom_and_signed_in_builtins() {
        let custom = vec![PiCustomProvider {
            id: "gw".into(),
            display_name: "Gateway".into(),
            models: vec![PiCustomModel {
                id: "m1".into(),
                name: String::new(),
                ..Default::default()
            }],
            ..Default::default()
        }];
        let mut anthropic = builtin("anthropic", true, PiAuthStatus::Oauth);
        anthropic.name = "Anthropic".into();
        anthropic.models = vec![discovered("claude-x")];
        let mut openai = builtin("openai", false, PiAuthStatus::None);
        openai.models = vec![discovered("gpt-x")];
        let builtin = vec![anthropic, openai];
        let groups = model_groups(&custom, Some(&builtin), &[], None);
        assert_eq!(
            groups,
            vec![
                ModelGroup {
                    provider: "gw".into(),
                    name: "Gateway".into(),
                    models: vec![("m1".into(), "m1".into())],
                },
                ModelGroup {
                    provider: "anthropic".into(),
                    name: "Anthropic".into(),
                    models: vec![("claude-x".into(), "claude-x".into())],
                },
            ]
        );
    }

    /// 凭据助手还没答上来时用快照里的目录；当前默认不在目录里也补进去，下拉里看得到它。
    #[test]
    fn model_groups_fall_back_and_keep_current_default() {
        let fallback = vec![PiModelChoiceGroup {
            provider: "deepseek".into(),
            provider_name: "deepseek".into(),
            models: vec![PiModelChoice {
                provider: "deepseek".into(),
                provider_name: "deepseek".into(),
                id: "deepseek-chat".into(),
                name: "deepseek-chat".into(),
            }],
        }];
        let current = model_ref("deepseek", "deepseek-new");
        let groups = model_groups(&[], None, &fallback, Some(&current));
        assert_eq!(
            groups[0].models,
            [
                ("deepseek-chat".to_string(), "deepseek-chat".to_string()),
                ("deepseek-new".to_string(), "deepseek-new".to_string()),
            ]
        );

        let groups = model_groups(&[], Some(&[]), &fallback, Some(&model_ref("x", "y")));
        assert_eq!(model_label(&groups, &model_ref("x", "y")), "x / y");
    }
}
