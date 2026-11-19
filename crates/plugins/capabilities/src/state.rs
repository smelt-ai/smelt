//! 能力库各表单、面板的状态类型。

use super::*;
use smelt_plugin_api::ui::kit::{TYPE, TypeStyled as _};

/// 「手动填写」默认模型：下拉里找不到的模型（还没进目录的新模型、改过地址的内置服务商）。
#[derive(Clone)]
pub struct PiModelEditor {
    pub provider: Entity<InputState>,
    pub model: Entity<InputState>,
    pub base_url: Entity<InputState>,
    pub api_key: Entity<InputState>,
}

impl PiModelEditor {
    /// 占位文字：建好时和换语言时都从这里取。
    pub fn label(&self, window: &mut Window, cx: &mut App) {
        set_placeholder(
            &self.provider,
            t!("model_editor.provider_placeholder"),
            window,
            cx,
        );
        set_placeholder(
            &self.model,
            t!("model_editor.model_placeholder"),
            window,
            cx,
        );
        set_placeholder(
            &self.base_url,
            t!("model_editor.base_url_placeholder"),
            window,
            cx,
        );
        set_placeholder(
            &self.api_key,
            t!("model_editor.key_placeholder"),
            window,
            cx,
        );
    }
}

#[derive(Clone)]
pub struct PiCustomModelEditor {
    pub id: Entity<InputState>,
    pub name: Entity<InputState>,
    pub context_window: Entity<InputState>,
    pub max_tokens: Entity<InputState>,
}

impl PiCustomModelEditor {
    /// 占位文字：建好时和换语言时都从这里取。
    pub fn label(&self, window: &mut Window, cx: &mut App) {
        set_placeholder(&self.id, t!("model_row.id_placeholder"), window, cx);
        set_placeholder(&self.name, t!("model_row.name_placeholder"), window, cx);
        set_placeholder(
            &self.context_window,
            t!("model_row.context_placeholder"),
            window,
            cx,
        );
        set_placeholder(
            &self.max_tokens,
            t!("model_row.max_tokens_placeholder"),
            window,
            cx,
        );
    }
}

#[derive(Clone)]
pub struct PiCustomProviderEditor {
    pub previous_id: Option<String>,
    pub id: Entity<InputState>,
    pub display_name: Entity<InputState>,
    pub api_key: Entity<InputState>,
    pub base_url: Entity<InputState>,
    pub api: String,
    pub models: Vec<PiCustomModelEditor>,
    /// 「获取模型列表」的进度与结果；`None` 表示还没点过。
    pub discovery: Option<ModelDiscoveryState>,
    pub credential_configured: bool,
}

impl PiCustomProviderEditor {
    /// 占位文字：建好时和换语言时都从这里取。
    pub fn label(&self, window: &mut Window, cx: &mut App) {
        set_placeholder(&self.id, t!("provider.id_placeholder"), window, cx);
        set_placeholder(
            &self.display_name,
            t!("provider.name_placeholder"),
            window,
            cx,
        );
        set_placeholder(&self.base_url, "https://gateway.example/v1", window, cx);
        let key = if self.credential_configured {
            t!("editor.key_configured")
        } else {
            t!("provider.key_placeholder")
        };
        set_placeholder(&self.api_key, key, window, cx);
        for model in &self.models {
            model.label(window, cx);
        }
    }
}

fn set_placeholder(
    input: &Entity<InputState>,
    text: impl Into<SharedString>,
    window: &mut Window,
    cx: &mut App,
) {
    let text = text.into();
    input.update(cx, |input, cx| input.set_placeholder(text, window, cx));
}

/// 模型发现的进度：一次直连 HTTP。
#[derive(Clone)]
pub enum ModelDiscoveryState {
    Loading,
    /// 端点答了。空列表也是答案——它说"我一个都不提供"。
    Ready(Vec<api::model::provider_api::DiscoveredModel>),
    /// 问不到。协议不支持列举、端点拒绝、凭据不对都落在这里，理由必须原样带出来。
    Failed(String),
}

/// Pi 内置 provider 的凭据面板：列表状态。
///
/// 列一次要起一个 bun 子进程（首次还可能连带装运行时），所以是异步填的，且
/// `None` 表示还没问过——面板不展开就不问，别让每次打开设置都多起一个进程。
#[derive(Clone)]
pub enum PiAuthProvidersState {
    Loading,
    Ready(Vec<api::model::pi_auth::PiAuthProvider>),
    Failed(String),
}

/// 等用户确认的服务商操作：删掉的配置、注销的登录都要重新填一遍，所以先问一句。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PiProviderConfirm {
    RemoveCustom(String),
    SignOut(String),
}

impl PiProviderConfirm {
    pub fn provider_id(&self) -> &str {
        match self {
            Self::RemoveCustom(id) | Self::SignOut(id) => id,
        }
    }
}

/// 登录流程当前在等用户回答的那个提问。
#[derive(Clone)]
pub struct PiLoginPrompt {
    pub id: String,
    pub kind: api::model::pi_auth::PiPromptKind,
    pub message: String,
    pub placeholder: Option<String>,
    pub options: Vec<api::model::pi_auth::PiPromptOption>,
}

/// 登录的是什么。两种登录在运行时那边是同一套会话（授权链接、粘贴、取消），面板也共用。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PiLoginTarget {
    /// Pi 内置 provider 的订阅 / API key 登录。
    Provider { id: String, name: String },
    /// `~/.pi/agent/mcp.json` 里一台要 OAuth 的 HTTP MCP 服务器。
    Mcp { name: String },
}

/// 一次进行中的（或刚结束的）登录。登录本身跑在运行时那边，这里只拿着它的 id。
#[derive(Clone)]
pub struct PiLoginView {
    pub target: PiLoginTarget,
    /// 运行时给的登录 id：取事件、回答、取消都带着它。`None` 是还在请运行时起登录
    /// （首次可能要先装运行时），面板先出来，点了不至于没反应。
    pub login_id: Option<String>,
    /// progress / info 的流水，最新的在最后。
    pub messages: Vec<String>,
    pub auth_url: Option<String>,
    pub instructions: Option<String>,
    /// device code 流：用户码 + 验证地址。
    pub device_code: Option<(String, String)>,
    pub prompt: Option<PiLoginPrompt>,
    /// 两个输入框在登录开始时就建好：提问是从子进程的事件流里来的，那里没有
    /// `Window`，建不了控件，也改不了掩码。按提问类型选用哪一个即可。
    pub input: Entity<InputState>,
    pub secret_input: Entity<InputState>,
    /// 输入框上的回车订阅。快照会 clone 整个 view，而订阅一旦被丢弃回车就失灵，
    /// 所以放进 `Rc` 跟着 view 一起活。订阅不是 `Send`，不能用 `Arc`。
    #[allow(dead_code, reason = "只为让订阅活到面板关闭，没有读取方")]
    pub input_subscriptions: std::rc::Rc<Vec<gpui::Subscription>>,
    /// 失败的原因；还在跑是 `None`。成功了面板直接收起，那一行的状态自己会变。
    pub error: Option<String>,
}

impl PiLoginView {
    /// 登录的是哪个内置 provider。
    pub fn provider_id(&self) -> Option<&str> {
        match &self.target {
            PiLoginTarget::Provider { id, .. } => Some(id),
            PiLoginTarget::Mcp { .. } => None,
        }
    }

    /// 登录的是哪台 MCP 服务器。
    pub fn mcp_server(&self) -> Option<&str> {
        match &self.target {
            PiLoginTarget::Mcp { name } => Some(name),
            PiLoginTarget::Provider { .. } => None,
        }
    }

    /// 当前提问该用哪个输入框。
    pub fn active_input(&self) -> Option<&Entity<InputState>> {
        match self.prompt.as_ref()?.kind {
            api::model::pi_auth::PiPromptKind::Select => None,
            api::model::pi_auth::PiPromptKind::Secret => Some(&self.secret_input),
            _ => Some(&self.input),
        }
    }
}

/// 画自定义 provider 的模型发现结果。
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_model_discovery_section(
    id_prefix: &'static str,
    state: Option<ModelDiscoveryState>,
    adopted: Vec<String>,
    manual_rows: usize,
    workspace: Entity<CapabilitiesPage>,
    adopt: fn(
        &mut CapabilitiesPage,
        api::model::provider_api::DiscoveredModel,
        &mut Window,
        &mut Context<CapabilitiesPage>,
    ),
    muted: gpui::Hsla,
    danger_fg: gpui::Hsla,
) -> gpui::AnyElement {
    let note = |color: gpui::Hsla, text: String| {
        div()
            .type_style(TYPE.text_xsmall)
            .text_color(color)
            .child(text)
    };

    match state {
        None if manual_rows == 0 => {
            note(muted, t!("discovery.auto").into_owned()).into_any_element()
        }
        None => note(muted, t!("discovery.hint").into_owned()).into_any_element(),
        Some(ModelDiscoveryState::Loading) => {
            note(muted, t!("discovery.loading").into_owned()).into_any_element()
        }
        Some(ModelDiscoveryState::Failed(error)) => note(
            danger_fg,
            t!("discovery.failed", error = error).into_owned(),
        )
        .into_any_element(),
        Some(ModelDiscoveryState::Ready(models)) if models.is_empty() => {
            note(muted, t!("discovery.empty").into_owned()).into_any_element()
        }
        Some(ModelDiscoveryState::Ready(models)) => {
            let total = models.len();
            let buttons = models.into_iter().map(move |model| {
                let entity = workspace.clone();
                let taken = adopted.iter().any(|id| id == &model.id);
                let label = model.label().to_string();
                Button::new(format!("{id_prefix}-discovered-{}", model.id))
                    .small()
                    .selected(taken)
                    .label(label)
                    .disabled(taken)
                    .on_click(move |_, window, cx| {
                        let model = model.clone();
                        entity.update(cx, |workspace, cx| {
                            adopt(workspace, model, window, cx);
                        });
                    })
            });
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(note(
                    muted,
                    t!("discovery.found", count = total).into_owned(),
                ))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .flex_wrap()
                        .children(buttons),
                )
                .into_any_element()
        }
    }
}
