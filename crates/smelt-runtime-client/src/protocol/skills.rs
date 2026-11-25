//! 技能管理（`skills.*`）的控制 RPC 契约：扫描、导入检查、预览计划与应用变更。
//!
//! 技能读写完全在目标机器的 `smeltd` 上执行，桌面与远程机器行为一致，UI 进程不直接碰磁盘。

use serde::{Deserialize, Serialize};

use super::control::{ControlCall, ControlMethod};

macro_rules! skills_calls {
    ($( $(#[$doc:meta])* $call:ident => $method:ident ($params:ty) -> $result:ty; )+) => {
        $(
            $(#[$doc])*
            pub enum $call {}

            impl ControlCall for $call {
                type Params = $params;
                type Result = $result;

                const METHOD: ControlMethod = ControlMethod::$method;
            }
        )+
    };
}

skills_calls! {
    /// 扫描目标机器用户级与可选项目级目录下的全部技能、覆盖情况与失效软链。
    SkillsScanCall => SkillsScan(SkillsScanParams) -> SkillsScanResult;
    /// 检查待导入目录里的 `SKILL.md`，校验名字并检测是否会遮盖用户级同名技能。
    SkillsInspectImportCall => SkillsInspectImport(SkillsInspectImportParams) -> SkillsSourceInfo;
    /// 预览调整作用范围或导入目录将要落盘的步骤列表（与实际执行走同一套规划逻辑）。
    SkillsPlanCall => SkillsPlan(SkillsPlanParams) -> SkillsPlanResult;
    /// 执行一次技能磁盘写操作（调范围、导入、补链接、移入回收目录、清理失效链接、解冲突、迁移旧目录）。
    SkillsApplyCall => SkillsApply(SkillsApplyParams) -> SkillsApplyResult;
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SkillsScanParams {
    /// 目标机器上当前项目的绝对路径；缺省或 `None` 时只扫用户级。
    #[serde(default)]
    pub project: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SkillAgentInfo {
    pub label: String,
    pub has_user_dir: bool,
    pub has_project_dir: bool,
    pub reads_agents_dir: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SkillPlacement {
    Universal,
    Legacy,
    Agent { label: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SkillAccess {
    Reads,
    Linked,
    Owner,
    Missing,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SkillCoverage {
    pub agent: String,
    pub access: SkillAccess,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SkillCopyInfo {
    pub name: String,
    pub description: String,
    pub dir: String,
    pub placement: SkillPlacement,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub project_scope: bool,
    pub base: String,
    pub dir: String,
    pub placement: SkillPlacement,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_ms: Option<i64>,
    pub bridges: Vec<String>,
    pub missing_bridges: Vec<String>,
    pub coverage: Vec<SkillCoverage>,
    pub conflicts: Vec<SkillCopyInfo>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BrokenSkillLink {
    pub path: String,
    pub project_scope: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SkillsScanResult {
    pub home: String,
    pub trash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    pub installed: Vec<String>,
    pub agents: Vec<SkillAgentInfo>,
    pub skills: Vec<SkillInfo>,
    pub broken: Vec<BrokenSkillLink>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SkillsInspectImportParams {
    #[serde(default)]
    pub project: Option<String>,
    pub source: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SkillsSourceInfo {
    pub name: String,
    pub description: String,
    pub has_global: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SkillScopeRequest {
    pub universal: bool,
    #[serde(default)]
    pub labels: Vec<String>,
    pub project_scope: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SkillsPlanParams {
    #[serde(default)]
    pub project: Option<String>,
    #[serde(flatten)]
    pub kind: SkillPlanKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SkillPlanKind {
    Scope {
        dir: String,
        request: SkillScopeRequest,
    },
    Import {
        source: String,
        request: SkillScopeRequest,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SkillAction {
    Install {
        from: String,
        to: String,
    },
    Move {
        from: String,
        to: String,
    },
    Link {
        at: String,
        to: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
    },
    Unlink {
        at: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
    },
    Trash {
        from: String,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SkillsPlanResult {
    pub actions: Vec<SkillAction>,
    pub replaces: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SkillsApplyParams {
    #[serde(default)]
    pub project: Option<String>,
    #[serde(flatten)]
    pub op: SkillApplyOp,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum SkillApplyOp {
    Scope {
        dir: String,
        request: SkillScopeRequest,
    },
    Import {
        source: String,
        request: SkillScopeRequest,
    },
    Repair {
        dir: String,
    },
    RepairAll,
    Trash {
        dir: String,
    },
    PruneBroken,
    ResolveConflict {
        dir: String,
        keep: String,
    },
    MigrateLegacy,
    EnsureTrash,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SkillMigrationResult {
    pub migrated: Vec<String>,
    pub skipped: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SkillsApplyResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migration: Option<SkillMigrationResult>,
}
