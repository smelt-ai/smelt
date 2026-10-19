//! `pi.auth.*` 的线上数据。登录会话在运行时。

use crate::pi_auth::{PiAuthProvider, PiLoginEvent, PiLoginMethod};
use crate::provider_api::DiscoveredModel;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvidersResult {
    pub providers: Vec<PiAuthProvider>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelsResult {
    pub models: Vec<DiscoveredModel>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginStartParams {
    pub provider_id: String,
    pub method: PiLoginMethod,
    /// 关掉助手的提问代答，每一问都交回给用户。
    pub ask_all: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginIdParams {
    pub login_id: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginEventsParams {
    pub login_id: String,
    /// 已经拿到的事件数；返回从这之后的。
    pub after: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginEventsResult {
    pub events: Vec<PiLoginEvent>,
    /// 登录已经结束，这批之后不会再有事件。
    pub finished: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginAnswerParams {
    pub login_id: String,
    pub prompt_id: String,
    pub value: String,
}
