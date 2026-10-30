//! 推送通知：把这台手机的推送设置同步到每台机器。
//!
//! 申请权限、拿 APNs 令牌、找推送中继换句柄都在 Dart / Swift 一侧做；这里只负责把句柄和
//! 要推的提醒类型登记到各台机器（连着的立刻，没连着的连上时），关掉时取消登记。

use flutter_rust_bridge::frb;
use smelt_model::remote_config::PushRegisterParams;

use crate::push::Desired;

/// 这台手机的推送登记。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushRegistration {
    /// 推送中继发的句柄。
    pub handle: String,
    /// 推送中继的地址，`https://` 开头。
    pub relay_url: String,
    pub approval: bool,
    pub input: bool,
    pub failure: bool,
}

/// 设推送：`None` 是关掉。每次启动都要设一次（句柄可能换了）；设之前不动机器上的登记。
/// 不阻塞，同步在后台进行，各台机器支不支持推送从机队流里看（`FleetRuntime::push_supported`）。
#[frb(sync)]
pub fn set_push_registration(registration: Option<PushRegistration>) {
    let desired = match registration {
        Some(registration) => Desired::On(PushRegisterParams {
            handle: registration.handle,
            relay_url: registration.relay_url,
            approval: registration.approval,
            input: registration.input,
            failure: registration.failure,
        }),
        None => Desired::Off,
    };
    crate::fleet::fleet().set_push(desired);
}
