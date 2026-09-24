# Smelt 架构改进路线图（Implementation Roadmap）

## 概览

基于架构审查的 P0/P1/P2 优先级，制定 13 周的改进方案。

---

## 第一阶段：P0 关键改进（Week 1-3）

### 目标
建立安全防线和测试框架

### 1. 远程访问安全加固（2 weeks）

#### 1.1 Token 管理改进

**当前状况：**
```rust
// ~/.smelt/remote-token 中的 token 直接用于认证
let token = fs::read_to_string("~/.smelt/remote-token")?;
socket.auth(token)?;  // ❌ 风险：同机程序可读取
```

**改进方案：**
```rust
// 两阶段 token 机制
pub enum RemoteToken {
    PairingToken {        // 配对用，一次性使用
        id: String,
        secret: String,
        created_at: Instant,
        expires_at: Instant,  // 5 分钟有效期
    },
    SessionKey {          // 长期连接用
        id: String,
        key: Secret,
        created_at: Instant,
    },
}

// 配对流程
// 1. 生成 PairingToken（随机 ID + 随机 secret）
// 2. 手机扫二维码 → 发 PairingToken
// 3. 验证成功 → 回复 SessionKey
// 4. PairingToken 立即失效
// 5. 后续连接使用 SessionKey
```

**实施步骤：**
1. 新增 `RemoteToken` enum（`crates/smelt-core/src/remote_token.rs`）
2. 修改 iroh/remote gateway 认证层
3. 添加 token 过期检查
4. 单测覆盖：正常流 + 过期流 + 重放攻击

**预计工作量：** 1.5 weeks
**涉及文件：**
- `crates/smelt-core/src/remote_token.rs` （新建）
- `crates/smeltd/src/remote.rs` （修改认证路径）
- `crates/smelt-iroh/src/lib.rs` （修改 iroh 握手）
- `crates/smelt-remote-gateway/src/lib.rs` （修改网关认证）

**测试用例：**
```rust
#[test]
fn test_pairing_token_one_time_use() {
    let token = PairingToken::new();
    assert!(validate_token(&token).is_ok());
    assert!(validate_token(&token).is_err());  // 第二次失败
}

#[test]
fn test_pairing_token_expiration() {
    let token = PairingToken::new_with_ttl(Duration::from_secs(5));
    sleep(Duration::from_secs(6));
    assert!(validate_token(&token).is_err());
}
```

#### 1.2 威胁模型文档化

**新增文件：** `docs/SECURITY.md`

**内容框架：**
```markdown
# Smelt 安全模型

## 威胁模型

### 假设防守的攻击（防线内）
- 同一机器的恶意程序
- 网络嗅探（iroh 中继窃听）
- 配对码泄露（一次性限制损害）

### 不防守的攻击（假设边界外）
- 本地物理访问（root 可读所有数据）
- 操作系统内核被攻击（假设 OS 可信）
- 云端服务商的恶意行为

## 安全性保证

### 本地安全
- Token 存储在 ~/.smelt/remote-token（权限 0600）
- SSH key 不会被复制到远程网关
- PTY 输出只发往授权客户端

### 网络安全
- iroh 所有连接基于 end-to-end 加密（Noise protocol）
- 配对码 5 分钟有效期
- Session key 定期轮换

### 多客户端安全
- 同一会话仅允许一个写权限客户端
- Watch 模式只读，互不影响
- 手机离线有宽限期（REMOTE_VIEWPORT_GRACE）
```

**预计工作量：** 0.5 weeks

---

### 2. E2E 测试框架（2 weeks）

#### 2.1 测试基础设施

**新增 module：** `crates/smelt/tests/e2e/`

**核心工具类：**
```rust
// crates/smelt/tests/e2e/env.rs
pub struct TestEnvironment {
    daemon: DaemonProcess,
    gui: GuiProcess,
    sandbox: TempDir,
}

impl TestEnvironment {
    pub async fn setup() -> Result<Self> {
        // 1. 创建隔离沙箱 ~/tmp/smelt-test-XXXX
        let sandbox = TempDir::new()?;
        
        // 2. 启动 daemon（测试模式）
        let daemon = DaemonProcess::spawn_with_config(
            DaemonConfig {
                data_dir: sandbox.path(),
                listen: "127.0.0.1:0",  // 随机端口
                headless: true,
            }
        ).await?;
        
        // 3. 启动 GUI（无头模式）
        let gui = GuiProcess::spawn_headless(
            GuiConfig {
                daemon_socket: daemon.socket_path(),
                data_dir: sandbox.path(),
            }
        ).await?;
        
        Ok(Self { daemon, gui, sandbox })
    }
    
    pub async fn create_session(&self, agent: &str) -> Result<SessionId> {
        self.gui.send_command(CreateSession { agent }).await
    }
    
    pub async fn edit_file(&self, path: &str, content: &str) -> Result<()> {
        fs::write(self.sandbox.path().join(path), content)?;
        self.wait_for_file_watcher().await?;
        Ok(())
    }
    
    pub async fn capture_acp_message(&self, session_id: SessionId) -> Result<AcpMessage> {
        // 通过 daemon 的 control API 获取最新消息
        timeout(Duration::from_secs(5), 
            self.daemon.wait_for_message(session_id)
        ).await?
    }
}
```

**GUI 无头模式支持：**
```rust
// crates/smelt/src/main.rs
#[derive(Clap)]
struct Args {
    #[clap(long)]
    headless: bool,  // 新增
    
    #[clap(long)]
    test_config: Option<PathBuf>,  // 新增
}

fn main() {
    if args.headless {
        // 跳过 GPUI 初始化，仅启动内核状态机
        run_headless_mode(args).await?;
    } else {
        // 正常 GUI 模式
        run_gui(args).await?;
    }
}
```

**预计工作量：** 1 week

#### 2.2 关键流程测试用例

**用例 1：编辑文件触发 ACP 提示**

```rust
#[tokio::test]
async fn e2e_file_edit_triggers_acp_prompt() {
    let env = TestEnvironment::setup().await?;
    
    // 创建测试项目
    env.create_project("test-proj").await?;
    env.create_session("pi").await?;
    
    // 编辑文件
    env.edit_file("main.rs", "fn main() {}").await?;
    
    // 等待 ACP 收到 prompt
    let msg = env.capture_acp_message(session_id).await?;
    
    assert_eq!(msg.role, "user");
    assert!(msg.content.contains("main.rs"));
    assert!(msg.content.contains("fn main()"));
    
    env.cleanup().await;
}
```

**用例 2：会话持久化**

```rust
#[tokio::test]
async fn e2e_session_survives_gui_restart() {
    let env = TestEnvironment::setup().await?;
    
    // 创建会话，输入命令
    let session_id = env.create_session("terminal").await?;
    env.send_input(&session_id, "echo hello\n").await?;
    
    // 等待输出
    let output = timeout(Duration::from_secs(2),
        env.capture_terminal_output(&session_id)
    ).await??;
    assert!(output.contains("hello"));
    
    // 杀死 GUI
    drop(env.gui);
    
    // 重启 GUI
    env.gui = GuiProcess::spawn_headless(...).await?;
    
    // 会话应该还在
    let output = env.capture_terminal_output(&session_id).await?;
    assert!(output.contains("hello"));  // 历史记录保留
    
    env.cleanup().await;
}
```

**用例 3：无缝升级（关键）**

```rust
#[tokio::test]
async fn e2e_upgrade_without_session_loss() {
    let env = TestEnvironment::setup().await?;
    
    // 创建活跃会话
    let session_id = env.create_session("terminal").await?;
    env.send_input(&session_id, "sleep 10\n").await?;
    
    // 等待输出部分内容
    sleep(Duration::from_secs(2)).await;
    
    // 执行升级
    env.daemon.upgrade(&new_binary_path).await?;
    
    // 验证会话继续运行
    sleep(Duration::from_secs(3)).await;
    let output = env.capture_terminal_output(&session_id).await?;
    // 应该能看到 "sleep 10" 的继续执行
    
    env.cleanup().await;
}
```

**用例 4：并发操作**

```rust
#[tokio::test]
async fn e2e_concurrent_sessions() {
    let env = TestEnvironment::setup().await?;
    
    // 创建 10 个并发会话
    let mut handles = vec![];
    for i in 0..10 {
        let session_id = env.create_session("terminal").await?;
        let env_clone = env.clone_for_test();
        
        handles.push(tokio::spawn(async move {
            for j in 0..5 {
                env_clone.send_input(&session_id, "echo test\n").await?;
                sleep(Duration::from_millis(100)).await;
            }
            Ok::<_, Error>(())
        }));
    }
    
    // 等待所有完成
    for h in handles {
        h.await??;
    }
    
    env.cleanup().await;
}
```

**预计工作量：** 1 week

#### 2.3 CI 集成

**修改：** `.github/workflows/test.yml`

```yaml
name: Test

on: [push, pull_request]

jobs:
  e2e-tests:
    runs-on: macos-latest
    steps:
      - uses: actions/checkout@v3
      
      - name: Install Rust
        uses: actions-rs/toolchain@v1
        with:
          toolchain: stable
      
      - name: Run unit tests
        run: make test
      
      - name: Run E2E tests
        run: cargo test --test "*" --features e2e
      
      - name: Upload test logs
        if: failure()
        uses: actions/upload-artifact@v3
        with:
          name: test-logs
          path: ~/.smelt/test-logs/
```

---

## 第二阶段：P1 重要改进（Week 4-9）

### 3. smelt-core 领域重构（4-6 weeks，分 3 个 sprint）

#### Sprint 1：准备与设计（Week 4）

**输出物：**
1. 新 crate 的 Cargo.toml 模板
2. 迁移清单（哪些 module 去哪个 crate）
3. 依赖关系图（新结构）

**行动：**
```bash
# 创建新 crate 框架
cargo new --lib crates/smelt-terminal-core
cargo new --lib crates/smelt-agent-core
cargo new --lib crates/smelt-automation-core
cargo new --lib crates/smelt-session-core
cargo new --lib crates/smelt-project-core
cargo new --lib crates/smelt-remote-core
```

**清单示例：**
```
smelt-terminal-core/
├─ term_text.rs
├─ terminal_theme.rs
├─ tty_color.rs
├─ osc.rs
└─ [共 8 个 module]

smelt-agent-core/
├─ acp_conn.rs
├─ acp_chat.rs
├─ acp_client.rs
├─ acp_session/
├─ agent_kind.rs
├─ agent_status.rs
├─ agent_bus.rs
├─ agent_definition.rs
├─ pi_rpc.rs
└─ [共 15 个 module]

... 其他 4 个 crate
```

#### Sprint 2：迁移执行（Week 5-6）

**每个 crate：**
1. 从 smelt-core 移出源代码
2. 更新 Cargo.toml 依赖
3. 修改导入语句（所有调用方）
4. 运行 `cargo check` 验证
5. 运行单元测试

**示例（以 smelt-terminal-core 为例）：**
```toml
# crates/smelt-terminal-core/Cargo.toml
[package]
name = "smelt-terminal-core"
version.workspace = true

[dependencies]
smelt-paths = { path = "../smelt-paths" }
smelt-store = { path = "../smelt-store" }
# ... 其他依赖
```

```rust
// crates/smelt-terminal-core/src/lib.rs
pub mod term_text;
pub mod terminal_theme;
pub mod tty_color;
pub mod osc;
// ... 其他 module

// 为了兼容性，可选择在原 smelt-core 中 re-export
```

**并行策略（加速）：**
- 3 个工程师分别处理 3 个 crate
- 第二周中期同步，处理跨 crate 依赖冲突
- 第三周初完成所有迁移

#### Sprint 3：验证与优化（Week 7）

**行动：**
1. 完整编译测试（所有 crate）
2. 所有单元测试通过
3. 无循环依赖
4. `cargo check` 无警告
5. 更新 Cargo.lock

**检查清单：**
```bash
# 验证命令
cargo build --workspace
cargo test --workspace
cargo clippy --all-targets
cargo doc --no-deps --open

# 生成依赖图
cargo tree --depth 3

# 检查循环依赖
cargo deny check
```

**预计工作量：** 4-6 weeks（3 个工程师并行，5 周完成）

---

### 4. 插件权限模型（3-4 weeks）

#### 4.1 权限系统设计

**新增文件：** `crates/smelt-plugin-api/src/permission.rs`

```rust
/// 权限类型（Capability-based）
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum PluginCapability {
    /// 只读：查看已有的 agent 定义
    ReadAgentDefinitions,
    
    /// 修改：创建新的 agent 定义
    CreateAgent,
    
    /// 修改：修改特定会话的内容
    ModifySession { session_id: SessionId },
    
    /// 修改：读/写特定目录
    FileAccess { 
        path: PathBuf,
        access: FileAccessMode,  // Read / Write / Execute
    },
    
    /// 网络：执行网络请求（可限制域名）
    NetworkAccess { 
        allowed_hosts: Option<Vec<String>>,  // None = 全部
    },
    
    /// 高级：调用特定 control API
    CallAPI { 
        method: String,  // e.g., "sessions.create"
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum FileAccessMode {
    Read,
    Write,
    Execute,
}

/// 插件清单中的权限声明
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PluginManifest {
    pub name: String,
    pub version: String,
    
    /// 插件声明的权限（用户安装时需审批）
    pub required_capabilities: Vec<PluginCapability>,
    
    /// 可选的附加权限（运行时提示）
    pub optional_capabilities: Vec<PluginCapability>,
}

/// 运行时权限检查
pub struct PermissionChecker {
    granted: Vec<PluginCapability>,
    audit_log: Vec<PermissionCheck>,
}

#[derive(Clone, Debug)]
pub struct PermissionCheck {
    pub timestamp: SystemTime,
    pub plugin_id: String,
    pub capability: PluginCapability,
    pub result: CheckResult,
}

pub enum CheckResult {
    Granted,
    Denied,
    UserPrompt,  // 可选权限，已提示用户
}

impl PermissionChecker {
    pub fn check(&mut self, 
        plugin_id: &str,
        capability: &PluginCapability
    ) -> Result<()> {
        // 1. 检查是否在 granted 列表
        if !self.granted.contains(capability) {
            self.audit_log.push(PermissionCheck {
                timestamp: SystemTime::now(),
                plugin_id: plugin_id.to_string(),
                capability: capability.clone(),
                result: CheckResult::Denied,
            });
            return Err("Permission denied".into());
        }
        
        // 2. 记录审计日志
        self.audit_log.push(PermissionCheck {
            timestamp: SystemTime::now(),
            plugin_id: plugin_id.to_string(),
            capability: capability.clone(),
            result: CheckResult::Granted,
        });
        
        Ok(())
    }
    
    pub fn grant(&mut self, capability: PluginCapability) {
        self.granted.push(capability);
    }
}
```

**预计工作量：** 1 week

#### 4.2 运行时集成

**修改：** `crates/smelt-plugin-host/src/lib.rs`

```rust
// 插件启动时
pub async fn spawn_plugin(
    manifest: PluginManifest,
    permissions: Vec<PluginCapability>,  // 用户审批的权限
) -> Result<PluginProcess> {
    let checker = PermissionChecker::new(permissions);
    
    let plugin = PluginProcess {
        id: manifest.name.clone(),
        checker,
        // ... 其他字段
    };
    
    Ok(plugin)
}

// API 调用时检查权限
impl PluginProcess {
    pub async fn call_api(&mut self, 
        method: &str, 
        params: serde_json::Value
    ) -> Result<serde_json::Value> {
        // 1. 检查权限
        self.checker.check(
            &self.id,
            &PluginCapability::CallAPI { 
                method: method.to_string() 
            }
        )?;
        
        // 2. 执行 API 调用
        match method {
            "sessions.create" => self.api_sessions_create(params).await,
            "sessions.list" => self.api_sessions_list().await,
            // ... 其他 API
            _ => Err("Unknown API".into()),
        }
    }
}
```

**预计工作量：** 1 week

#### 4.3 用户界面与审批

**新增 UI：** 插件安装向导

```
┌─────────────────────────────────────────────────┐
│ 插件：my-awesome-plugin v1.0.0                  │
│                                                 │
│ 请求以下权限：                                    │
│  ☑ 查看 Agent 定义 (ReadAgentDefinitions)      │
│  ☑ 创建 Agent (CreateAgent)                     │
│  ☑ 网络访问 (NetworkAccess) [api.example.com]  │
│  ☑ 调用 API: sessions.create                    │
│                                                 │
│ 审计日志保存于 ~/.smelt/plugin-audit.log        │
│                                                 │
│           [拒绝]              [允许]             │
└─────────────────────────────────────────────────┘
```

**修改：** `crates/smelt/src/settings/plugins.rs`

```rust
pub struct PluginInstallDialog {
    manifest: PluginManifest,
    approved_capabilities: Vec<PluginCapability>,
}

impl PluginInstallDialog {
    pub fn render(&self, cx: &mut ViewContext<Self>) -> impl Element {
        v_flex()
            .child(h1(format!("{}  v{}", self.manifest.name, self.manifest.version)))
            .child("请求以下权限：")
            .child(
                v_flex()
                    .children(
                        self.manifest.required_capabilities.iter().map(|cap| {
                            checkbox(format!("{:?}", cap))
                                .checked(true)
                                .on_toggle(move |checked| {
                                    // 更新 approved_capabilities
                                })
                        })
                    )
            )
            .child(h_flex()
                .child(Button::new("deny", "拒绝").on_click(|_| Action::Deny))
                .child(Button::new("approve", "允许").on_click(|_| Action::Approve))
            )
    }
}
```

**预计工作量：** 1.5 weeks

#### 4.4 测试和文档

**单元测试：** `crates/smelt-plugin-api/src/permission.rs` 的测试

```rust
#[test]
fn test_permission_grant_and_check() {
    let mut checker = PermissionChecker::new(vec![]);
    
    // 未授予权限，应拒绝
    assert!(checker.check("plugin1", 
        &PluginCapability::ReadAgentDefinitions).is_err());
    
    // 授予权限后应通过
    checker.grant(PluginCapability::ReadAgentDefinitions);
    assert!(checker.check("plugin1", 
        &PluginCapability::ReadAgentDefinitions).is_ok());
}

#[test]
fn test_audit_log() {
    let mut checker = PermissionChecker::new(
        vec![PluginCapability::ReadAgentDefinitions]
    );
    
    let _ = checker.check("plugin1", &PluginCapability::ReadAgentDefinitions);
    
    assert_eq!(checker.audit_log.len(), 1);
    assert_eq!(checker.audit_log[0].result, CheckResult::Granted);
}
```

**文档：** `docs/plugin-security.md`

```markdown
# 插件安全模型

## 权限声明

插件必须在 plugin.json 中声明所需权限：

\`\`\`json
{
  "name": "my-plugin",
  "requiredCapabilities": [
    "ReadAgentDefinitions",
    { "name": "CallAPI", "method": "sessions.list" }
  ]
}
\`\`\`

## 审批流程

1. 用户下载插件
2. 显示权限确认对话
3. 用户选择批准或拒绝
4. 若批准，权限记录到审计日志
5. 插件运行时每次 API 调用都检查权限

## 审计日志

所有权限检查都记录在 ~/.smelt/plugin-audit.log：

\`\`\`
2025-01-23T10:30:45Z | plugin1 | ReadAgentDefinitions | GRANTED
2025-01-23T10:30:46Z | plugin1 | NetworkAccess | DENIED
\`\`\`
```

**预计工作量：** 0.5 weeks

---

### 5. 数据隐私政策（1-2 weeks）

**新增文件：** `PRIVACY.md` 和 `docs/SECURITY.md`

**内容清单：**

1. **PRIVACY.md**（用户级）
   - 什么数据留在本机
   - 什么数据上报给第三方
   - 数据保留时间
   - 用户的隐私权利

2. **docs/SECURITY.md**（技术细节）
   - 威胁模型
   - 安全保证
   - 已知的局限性
   - 报告安全漏洞的流程

**预计工作量：** 1-2 weeks

---

## 第三阶段：P2 质量改善（Week 10-13）

### 6. 日志系统完善（1 week）

**修改：** `crates/smelt-core/src/app_log.rs`

```rust
use tracing::{Level, metadata::LevelFilter};
use tracing_subscriber::{fmt, layer::SubscriberExt};

pub struct LogConfig {
    pub level: LevelFilter,
    pub format: LogFormat,
    pub rotation: LogRotation,
    pub output: LogOutput,
}

pub enum LogFormat {
    Text,    // 人类可读
    Json,    // 结构化日志
}

pub enum LogRotation {
    Daily,
    Size(u64),  // 按大小轮转
    None,
}

pub enum LogOutput {
    File(PathBuf),
    Stderr,
    Both,
}

pub fn init_logging(config: LogConfig) -> Result<()> {
    let registry = tracing_subscriber::registry()
        .with(
            fmt::layer()
                .with_max_level(config.level)
                .with_thread_ids(true)
                .with_target(true)
        )
        .with(
            match config.format {
                LogFormat::Json => {
                    fmt::layer()
                        .json()
                        .with_max_level(config.level)
                        .boxed()
                }
                LogFormat::Text => {
                    fmt::layer()
                        .pretty()
                        .with_max_level(config.level)
                        .boxed()
                }
            }
        );
    
    // 可选：文件输出
    if matches!(config.output, LogOutput::File(_) | LogOutput::Both) {
        let file = File::create("~/.smelt/smelt.log")?;
        // ... 添加文件 layer
    }
    
    tracing::subscriber::set_global_default(registry)?;
    Ok(())
}
```

**预计工作量：** 1 week

### 7. 发版流程自动化（1 week）

**新增文件：** `CHANGELOG.md` 和 `scripts/changelog-gen.sh`

**CHANGELOG.md 格式（Keep a Changelog）：**

```markdown
# Changelog

All notable changes to this project will be documented in this file.

## [0.9.1] - 2025-01-23

### Added
- Remote access security improvements (one-time pairing token)
- E2E test framework for full workflow testing
- Plugin permission model (capability-based)

### Fixed
- Fixed race condition in terminal output handling
- Improved daemon upgrade reliability

### Changed
- Restructured smelt-core into domain-specific crates
- Updated GPUI to latest commit

## [0.9.0] - 2024-12-15
...
```

**自动生成脚本：**

```bash
#!/bin/bash
# scripts/changelog-gen.sh
# 从 git log 自动生成 CHANGELOG.md

VERSION=$1
PREV_TAG=$(git describe --tags --abbrev=0)

echo "## [$VERSION] - $(date +%Y-%m-%d)" >> CHANGELOG.md
echo >> CHANGELOG.md

echo "### Added" >> CHANGELOG.md
git log $PREV_TAG..HEAD --grep="feat" --oneline | \
    sed 's/^/- /' >> CHANGELOG.md

echo "### Fixed" >> CHANGELOG.md
git log $PREV_TAG..HEAD --grep="fix" --oneline | \
    sed 's/^/- /' >> CHANGELOG.md

# 自动 git tag 和 push
git add CHANGELOG.md
git commit -m "chore: release $VERSION"
git tag -a "v$VERSION" -m "Release $VERSION"
git push origin --tags
```

**预计工作量：** 1 week

### 8. Metrics/Profiling（1-2 weeks）

**新增文件：** `crates/smelt-core/src/metrics.rs`

```rust
use prometheus::{
    Counter, Histogram, Registry, IntCounter, IntHistogram,
};

pub struct Metrics {
    pub socket_latency: Histogram,
    pub grid_updates: Counter,
    pub memory_usage: Gauge,
    pub acp_messages: IntCounter,
}

lazy_static! {
    pub static ref METRICS = {
        let registry = Registry::new();
        
        let socket_latency = Histogram::new(
            "socket_latency_ms",
            "Socket communication latency in milliseconds"
        ).unwrap();
        
        let grid_updates = Counter::new(
            "terminal_grid_updates",
            "Terminal grid update count"
        ).unwrap();
        
        let memory_usage = Gauge::new(
            "memory_usage_bytes",
            "Current memory usage"
        ).unwrap();
        
        registry.register(Box::new(socket_latency.clone())).ok();
        registry.register(Box::new(grid_updates.clone())).ok();
        registry.register(Box::new(memory_usage.clone())).ok();
        
        Metrics {
            socket_latency,
            grid_updates,
            memory_usage,
            acp_messages,
        }
    };
}

// 在关键路径采集
pub fn measure_socket_latency<F, T>(f: F) -> T
where
    F: FnOnce() -> T,
{
    let start = Instant::now();
    let result = f();
    let elapsed = start.elapsed().as_secs_f64() * 1000.0;
    METRICS.socket_latency.observe(elapsed);
    result
}
```

**预计工作量：** 1-2 weeks

---

## P3 持续改进项目

### 9. 架构决策记录（ADR）

**新增目录：** `docs/adr/`

**模板：** `docs/adr/NNNN-title.md`

```markdown
# ADR-001: 选择 GPUI 而非 Qt/Tauri

## 状态
已接受

## 背景
需要选择 macOS 桌面应用的 GUI 框架。

### 候选方案
1. Qt - 跨平台，生态成熟
2. Tauri - Rust 原生，较新
3. GPUI - Zed 的 GPU 加速框架

## 决策
选择 GPUI，理由：
- GPU 加速，性能好
- 与 Zed 社区紧密合作
- Rust 原生，类型安全
- 渲染速度最快

## 后果
- 依赖 Zed 项目维护
- macOS 专有（当前不跨平台）
- 社区较小，文档少

## 备选方案回顾
- Qt：大而全，但非 Rust 原生
- Tauri：可行，但渲染速度不如 GPUI
```

**存档其他重要决策：**
- ADR-002: Daemon 架构（PTY 在 daemon，不在 GUI）
- ADR-003: SQLite vs JSON 存储
- ADR-004: 无缝升级的两阶段提交
- ADR-005: 事件驱动状态管理

### 10. Cargo.toml 清理

**检查清单：**
```toml
# 为每个依赖补充注释，说明为什么需要它

[dependencies]
# 终端仿真：ANSI 解析 + 网格模型
alacritty_terminal = { version = "0.26", default-features = false }

# ACP 对话协议客户端（锁 2.1 防漂移）
agent-client-protocol = { version = "=2.1.0", features = ["unstable_plan_operations"] }

# 异步运行时（全功能）
tokio = { version = "1", features = ["full"] }

# ... 为每个都补充注释
```

---

## 总时间表

```
Week 1-3:  P0 关键改进
  ├─ Week 1-2: 远程安全加固
  └─ Week 2-3: E2E 测试框架

Week 4-9:  P1 重要改进
  ├─ Week 4:   smelt-core 重构准备
  ├─ Week 5-6: smelt-core 迁移执行
  ├─ Week 7:   smelt-core 验证
  └─ Week 8-9: 插件权限模型 + 数据隐私

Week 10-13: P2 质量改善
  ├─ Week 10:  日志系统 + 发版自动化
  └─ Week 11-13: Metrics + ADR 补充

总计：13 weeks（约 3 个月，3 人工程师并行）
```

---

## 资源需求

| 阶段 | 工程师数 | 工作量 | 预计耗时 |
|------|---------|--------|---------|
| P0 | 2 | 4 weeks | 2 weeks（并行）|
| P1 | 3 | 12 weeks | 4 weeks（并行）|
| P2 | 2 | 4 weeks | 2 weeks（并行）|
| P3 | 1 (兼职) | 持续 | 持续 |

---

## 质量保障

### 每个 PR 的检查清单
- [ ] `make check-all` 通过
- [ ] 新增代码有单测
- [ ] 文档/注释已更新
- [ ] commit 消息遵循 Conventional Commits
- [ ] 若有 breaking change，更新 CHANGELOG.md

### 发布前检查
- [ ] 所有 P0 改进已完成
- [ ] E2E 测试全部通过
- [ ] code coverage ≥ 80%
- [ ] 没有 `clippy` 警告
- [ ] CHANGELOG.md 已更新

---

## 成功指标

| 指标 | 目标 | 衡量方法 |
|------|------|---------|
| 远程安全 | 通过 security audit | 第三方审计 |
| 测试覆盖 | E2E 覆盖 5 个关键流程 | 测试用例 count |
| 代码质量 | 无循环依赖，无 clippy 警告 | CI 检查 |
| 文档完整 | ADR 覆盖 5 个重大决策 | 文档齐全性 |
| 用户体验 | 安装插件时清晰看到权限 | 用户反馈 |

