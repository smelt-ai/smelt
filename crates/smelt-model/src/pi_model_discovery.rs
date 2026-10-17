//! 问端点有哪些模型时的请求和地址校验。发请求在运行时。

/// 拼出 `GET` 模型列表的完整 URL。
///
/// 用户填 base URL 时带不带 `/v1`、带不带尾斜杠都有；已经指到 `/models` 的也
/// 有（有些网关文档就是这么给的）。这里全部收敛到一个形状，避免拼出
/// `.../v1//models` 或 `.../models/models` 这种一定 404 的地址。
pub fn models_endpoint(base_url: &str) -> Result<String, String> {
    let base = base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err("请先填写 API 地址".to_string());
    }
    if !base.starts_with("http://") && !base.starts_with("https://") {
        return Err(format!("API 地址需要以 http:// 或 https:// 开头：{base}"));
    }
    if base.ends_with("/models") {
        return Ok(base.to_string());
    }
    Ok(format!("{base}/models"))
}

/// 一次发现请求。`api_key` 为空表示端点不需要凭据（有些内网网关如此）。
#[derive(Clone, Debug, Default)]
pub struct ModelDiscoveryRequest {
    pub base_url: String,
    pub api: String,
    pub api_key: String,
}
