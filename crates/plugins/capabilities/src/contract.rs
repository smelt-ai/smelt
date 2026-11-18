//! 能力库（来源 `smelt.capabilities`）对外的约定：能力库页的视图 id。
//!
//! 这里是权威定义。插件之间不互相依赖，插件接口也不认识具体功能：调用方各自照抄用到的
//! id 和字段，注明出处（JS 插件按同样的字符串和 JSON 调）。改这里就是改线上形状，同一次
//! 改动里把照抄的各处一起改掉（`grep -rn 'smelt.capabilities/' crates plugins`）。

/// 能力库页：给 Pi 装的东西。打开时的参数都可以不给：`{"tab": "plugins" | "mcp",
/// "machine": …}`。`machine` 不给或已经移除时停在本机。
pub(crate) const VIEW: &str = "smelt.capabilities/main";

// 设置里的两页，参数都是 `{"machine": …}`，可以不给：
// - `smelt.capabilities/models`：Provider 凭据、默认模型、思考强度。
// - `smelt.capabilities/memory`：记忆检索的向量和重排服务。
