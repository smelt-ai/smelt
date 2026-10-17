//! 远程访问配置。存取在运行时。


/// 远程访问开关。字段语义与 GUI 设置页一一对应。
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RemoteConfig {
    /// 用户是否希望远程访问开着。守护启动时按它决定要不要自动拉起网关和隧道。
    #[serde(default)]
    pub enabled: bool,
    /// 用户自己的 iroh relay。空值表示未配置，不会回退到公共 relay。
    #[serde(default)]
    pub iroh_relay: String,
    /// 这条链接是否允许 approve/deny/reply。`#[serde(default)]`：比 `enabled`
    /// 更晚加，旧配置缺省按只读处理——不能让老用户的配置在升级后突然变成可写。
    #[serde(default)]
    pub write_enabled: bool,
}
