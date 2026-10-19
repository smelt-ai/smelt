//! 桌面让远程运行时升到指定版本（设计文档 D7）。运行时自己去 GitHub Release 下载那个
//! 版本的包，校验、装好，再无缝交接到新版本；桌面只说要哪个版本，不传字节。
//!
//! 这是冻结契约：不走带 `control_version` 的控制信封，因为要升级的恰恰是控制协议对不上
//! 的运行时，信封一对不上就被拒。连接首行 `{"op":"runtime_upgrade","version":"0.9.1"}`，
//! 运行时逐行回 [`RuntimeUpgradeReply`]：先回 `accepted`，再回一个结局；交接成功时旧进程
//! 直接退出，连接就此断开，客户端重新探测看版本。一行都没回就断开，是运行时太旧、不认识
//! 这个 op。形状只能加可缺省的字段，不能改。

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeUpgradeRequest {
    /// 要升到的版本，和 Release 的 tag 去掉 `v` 一致，如 `0.9.1`、`0.9.1-rc.1`。
    pub version: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RuntimeUpgradeReply {
    /// 接下了，开始下载。之后还有一行结局，或者交接成功、连接断开。
    Accepted,
    /// 装着的已经是这个版本，什么都没做。
    Current,
    /// 新版本装好了，但运行时正忙（agent 回合没完等），空闲时自己换过去。
    Staged { reason: String },
    Failed { error: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 旧桌面、新运行时之间靠这几行通话，字段名和取值都不能变。
    #[test]
    fn replies_are_a_frozen_wire_contract() {
        let cases = [
            (RuntimeUpgradeReply::Accepted, r#"{"state":"accepted"}"#),
            (RuntimeUpgradeReply::Current, r#"{"state":"current"}"#),
            (
                RuntimeUpgradeReply::Staged {
                    reason: "忙".into(),
                },
                r#"{"state":"staged","reason":"忙"}"#,
            ),
            (
                RuntimeUpgradeReply::Failed {
                    error: "坏了".into(),
                },
                r#"{"state":"failed","error":"坏了"}"#,
            ),
        ];
        for (reply, wire) in cases {
            assert_eq!(serde_json::to_string(&reply).unwrap(), wire);
            assert_eq!(
                serde_json::from_str::<RuntimeUpgradeReply>(wire).unwrap(),
                reply
            );
        }
        assert_eq!(
            serde_json::to_value(RuntimeUpgradeRequest {
                version: "0.9.1".into()
            })
            .unwrap(),
            serde_json::json!({ "version": "0.9.1" })
        );
    }
}
