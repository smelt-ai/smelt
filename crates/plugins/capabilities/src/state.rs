//! 能力库各表单、面板的状态类型。

use super::*;

#[derive(Clone)]
pub struct PiModelEditor {
    pub provider: Entity<gpui_component::input::InputState>,
    pub model: Entity<gpui_component::input::InputState>,
    pub base_url: Entity<gpui_component::input::InputState>,
    pub api_key: Entity<gpui_component::input::InputState>,
    pub thinking_level: String,
    pub credential_configured: bool,
}

#[derive(Clone)]
pub struct PiCustomModelEditor {
    pub id: Entity<gpui_component::input::InputState>,
    pub name: Entity<gpui_component::input::InputState>,
    pub context_window: Entity<gpui_component::input::InputState>,
    pub max_tokens: Entity<gpui_component::input::InputState>,
}

#[derive(Clone)]
pub struct PiCustomProviderEditor {
    pub previous_id: Option<String>,
    pub id: Entity<gpui_component::input::InputState>,
    pub display_name: Entity<gpui_component::input::InputState>,
    pub api_key: Entity<gpui_component::input::InputState>,
    pub base_url: Entity<gpui_component::input::InputState>,
    pub api: String,
    pub models: Vec<PiCustomModelEditor>,
    /// 「获取模型列表」的进度与结果；`None` 表示还没点过。
    pub discovery: Option<ModelDiscoveryState>,
    pub credential_configured: bool,
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

/// 「换个默认模型」的选择器：某个已配置 provider 现在能用的模型。
///
/// 和 [`PiLoginView::models`] 分开，是因为两者的生命周期不同：那份跟着一次登录
/// 会话走，登录面板一关就没了；这份是随时可以对任意一个已登录 provider 打开的。
#[derive(Clone)]
pub struct PiAuthModelPicker {
    pub provider_id: String,
    pub provider_name: String,
    /// `None` 表示还在问。
    pub models: Option<Result<Vec<api::model::provider_api::DiscoveredModel>, String>>,
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

/// 一次进行中的（或刚结束的）登录。登录本身跑在运行时那边，这里只拿着它的 id。
#[derive(Clone)]
pub struct PiLoginView {
    pub provider_id: String,
    pub provider_name: String,
    /// 运行时给的登录 id：取事件、回答、取消都带着它。
    pub login_id: String,
    /// progress / info 的流水，最新的在最后。
    pub messages: Vec<String>,
    pub auth_url: Option<String>,
    pub instructions: Option<String>,
    /// device code 流：用户码 + 验证地址。
    pub device_code: Option<(String, String)>,
    pub prompt: Option<PiLoginPrompt>,
    /// 两个输入框在登录开始时就建好：提问是从子进程的事件流里来的，那里没有
    /// `Window`，建不了控件，也改不了掩码。按提问类型选用哪一个即可。
    pub input: Entity<gpui_component::input::InputState>,
    pub secret_input: Entity<gpui_component::input::InputState>,
    /// 输入框上的回车订阅。快照会 clone 整个 view，而订阅一旦被丢弃回车就失灵，
    /// 所以放进 `Rc` 跟着 view 一起活。订阅不是 `Send`，不能用 `Arc`。
    #[allow(dead_code, reason = "只为让订阅活到面板关闭，没有读取方")]
    pub input_subscriptions: std::rc::Rc<Vec<gpui::Subscription>>,
    /// `None` 表示还在跑。
    pub outcome: Option<Result<(), String>>,
    /// 登录成功后可选的模型目录，用来一键设默认。
    pub models: Option<Result<Vec<api::model::provider_api::DiscoveredModel>, String>>,
}

impl PiLoginView {
    /// 当前提问该用哪个输入框。
    pub fn active_input(&self) -> Option<&Entity<gpui_component::input::InputState>> {
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
    let note = |color: gpui::Hsla, text: String| div().text_xs().text_color(color).child(text);

    match state {
        None if manual_rows == 0 => note(
            muted,
            "未列出任何模型：保存时将从 API 地址自动获取，并在每次启动时刷新。\
             想自己固定一份目录，就在上面添加模型。"
                .to_string(),
        )
        .into_any_element(),
        None => note(
            muted,
            "可以点「获取模型列表」从 API 地址读取该服务提供的模型，不必逐个手填。".to_string(),
        )
        .into_any_element(),
        Some(ModelDiscoveryState::Loading) => {
            note(muted, "正在向该端点询问可用模型…".to_string()).into_any_element()
        }
        Some(ModelDiscoveryState::Failed(error)) => note(
            danger_fg,
            format!("未能读取模型列表：{error}（仍可在下方手动填写）"),
        )
        .into_any_element(),
        Some(ModelDiscoveryState::Ready(models)) if models.is_empty() => note(
            muted,
            "该端点没有报告任何模型，请在下方手动填写。".to_string(),
        )
        .into_any_element(),
        Some(ModelDiscoveryState::Ready(models)) => {
            let total = models.len();
            let buttons = models.into_iter().map(move |model| {
                let entity = workspace.clone();
                let taken = adopted.iter().any(|id| id == &model.id);
                let label = model.label().to_string();
                Button::new(format!("{id_prefix}-discovered-{}", model.id))
                    .choice()
                    .compact()
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
            v_flex()
                .gap_1()
                .child(note(
                    muted,
                    format!("该端点提供 {total} 个模型，点击添加到下方列表："),
                ))
                .child(h_flex().gap_2().flex_wrap().children(buttons))
                .into_any_element()
        }
    }
}
