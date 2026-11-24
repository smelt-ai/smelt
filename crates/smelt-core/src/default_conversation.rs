//! 侧栏「新对话」的默认智能体。
//!
//! 这是一次明确选择，不是「最近用过」或「列表第一个」。这里只管存取；没选过、选的
//! 已删掉或不能开对话时怎么办，由智能体插件（`plugins/agents`）决定。

/// 读出侧栏默认智能体。读失败时视为没设过，不阻塞界面。由运行时调用，
/// 随 `agent.host.*` 一起交给客户端。
pub fn load_on(store: &smelt_store::Store) -> Option<String> {
    match store.default_conversation_agent() {
        Ok(id) => id
            .map(|id| id.trim().to_string())
            .filter(|id| !id.is_empty()),
        Err(error) => {
            eprintln!("[default-conversation] 读取默认智能体失败，视为未设置: {error}");
            None
        }
    }
}

/// 设为 `Some` 记下默认智能体，`None` 清掉。
pub fn store_on(store: &smelt_store::Store, agent_id: Option<&str>) -> Result<(), String> {
    store
        .put_default_conversation_agent(agent_id.map(str::trim).filter(|id| !id.is_empty()))
        .map_err(|error| error.to_string())
}
