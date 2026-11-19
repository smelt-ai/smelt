//! JS 插件里运行时数据的类型声明：控制接口每个方法的参数和结果，`session_states`、
//! `workspace_directory` 交回的形状。由 Rust 线上类型的 JSON Schema（schemars）生成
//! TypeScript，提交在 `control_types.d.ts`，接在 `smelt-plugin-api` 模块的声明后面。
//!
//! 参数按运行时收什么（反序列化）生成，结果按运行时发什么（序列化）生成。同一个类型两边形状
//! 不同（带默认值的字段收的时候可省、发的时候总在）时，收的那份另叫 `<名字>Input`。
//!
//! 整个模块只在测试里编：生成和比对都是测试，App 只 `include_str!` 那份文件。
//!
//! 改了线上类型，这里的测试报声明过期，重新生成再提交：
//! `SMELT_UPDATE_TYPES=1 cargo test -j 2 -p smelt-plugin-api --features js --lib js::control_types`

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use schemars::SchemaGenerator;
use schemars::generate::SchemaSettings;
use serde_json::{Map, Value};

use crate::client::protocol::control::{ControlCall, ControlMethod};

/// 生成的文件放在哪，相对本 crate。
const FILE: &str = "src/js/control_types.d.ts";

/// 一个方法：线上名，参数（收的一边）和结果（发的一边）的 schema。
struct Method {
    method: ControlMethod,
    params: Value,
    result: Value,
}

struct Generators {
    input: SchemaGenerator,
    output: SchemaGenerator,
}

impl Generators {
    fn new() -> Self {
        let settings =
            |settings: SchemaSettings| settings.with(|settings| settings.meta_schema = None);
        Self {
            input: settings(SchemaSettings::draft2020_12().for_deserialize()).into_generator(),
            output: settings(SchemaSettings::draft2020_12().for_serialize()).into_generator(),
        }
    }

    fn method<C: ControlCall>(&mut self) -> Method {
        Method {
            method: C::METHOD,
            params: self.input.subschema_for::<C::Params>().to_value(),
            result: self.output.subschema_for::<C::Result>().to_value(),
        }
    }
}

macro_rules! control_calls {
    ($($call:path,)+) => {
        fn methods(generators: &mut Generators) -> Vec<Method> {
            vec![$(generators.method::<$call>(),)+]
        }
    };
}

// 控制接口的全部方法，按 `ControlMethod` 的次序。漏了的测试会报。
control_calls! {
    crate::client::protocol::control::SystemDescribe,
    crate::model::control_api::SessionList,
    crate::model::control_api::AgentMemoryMeta,
    crate::model::control_api::AgentMemoryRecordRead,
    crate::model::control_api::AgentMemoryRecordWrite,
    crate::model::control_api::AgentDefinitionList,
    crate::model::control_api::AgentDefinitionGet,
    crate::model::control_api::AgentDefinitionCreateCall,
    crate::model::control_api::AgentDefinitionUpdateCall,
    crate::model::control_api::AgentDefinitionDelete,
    crate::model::control_api::SettingsGet,
    crate::model::control_api::SettingsUpdate,
    crate::model::control_api::AutomationList,
    crate::model::control_api::AutomationGet,
    crate::model::control_api::AutomationUpsertCall,
    crate::model::control_api::AutomationDelete,
    crate::model::control_api::AutomationSetEnabled,
    crate::model::control_api::AutomationRunOnce,
    crate::client::protocol::git::GitDiscoverCall,
    crate::client::protocol::git::GitRepoInfoCall,
    crate::client::protocol::git::GitStatusCall,
    crate::client::protocol::git::GitBranchesCall,
    crate::client::protocol::git::GitCheckoutCall,
    crate::client::protocol::git::GitBranchDeleteCall,
    crate::client::protocol::git::GitMergeCall,
    crate::client::protocol::git::GitStageCall,
    crate::client::protocol::git::GitUnstageCall,
    crate::client::protocol::git::GitDiscardFileCall,
    crate::client::protocol::git::GitApplyPatchCall,
    crate::client::protocol::git::GitDiscardAllCall,
    crate::client::protocol::git::GitCommitCall,
    crate::client::protocol::git::GitPushCall,
    crate::client::protocol::git::GitFetchCall,
    crate::client::protocol::git::GitPullCall,
    crate::client::protocol::git::GitStashPushCall,
    crate::client::protocol::git::GitStashPopCall,
    crate::client::protocol::git::GitFileDiffCall,
    crate::client::protocol::git::GitAllDiffCall,
    crate::client::protocol::git::GitLogCall,
    crate::client::protocol::git::GitCommitFilesCall,
    crate::client::protocol::git::GitCommitFileDiffCall,
    crate::client::protocol::files::ProjectTreeCall,
    crate::client::protocol::files::ProjectSearchCall,
    crate::client::protocol::files::ProjectDeleteEntryCall,
    crate::client::protocol::files::FileOpenCall,
    crate::client::protocol::files::FileSaveCall,
    crate::client::protocol::files::SessionCompleteFilesCall,
    crate::client::protocol::history::HistoryListCall,
    crate::client::protocol::history::HistoryDetailCall,
    crate::client::protocol::history::HistoryDeleteCall,
    crate::model::control_api::AgentHostGet,
    crate::model::control_api::AgentHostApplyEdit,
    crate::model::control_api::LaunchGet,
    crate::model::control_api::LaunchReplace,
    crate::model::control_api::MemoryTestEmbedding,
    crate::model::control_api::WorkspaceConversationCreate,
    crate::model::control_api::WorkspaceConversationUpdate,
    crate::model::control_api::WorkspaceConversationDelete,
    crate::model::control_api::WorkspaceConversationArchive,
    crate::model::control_api::WorkspaceProjectOpen,
    crate::model::control_api::WorkspaceProjectClose,
    crate::model::control_api::WorkspaceProjectRepositoryAction,
    crate::model::control_api::WorkspaceWorksetCreate,
    crate::model::control_api::WorkspaceWorksetRename,
    crate::model::control_api::WorkspaceWorksetInspect,
    crate::model::control_api::WorkspaceWorksetFinish,
    crate::model::control_api::WorkspaceDirectoryGet,
    crate::model::control_api::WorkspaceRename,
    crate::model::control_api::AgentMemoryList,
    crate::model::control_api::AgentMemoryDelete,
    crate::model::control_api::AgentMemoryApprove,
    crate::model::control_api::AutomationRunDetail,
    crate::model::control_api::AgentSpaceEnsure,
    crate::model::control_api::AgentHooksStatus,
    crate::model::control_api::AgentHooksInstall,
    crate::model::control_api::AgentHooksUninstall,
    crate::model::control_api::AgentCliInspect,
    crate::model::control_api::AgentCliInstall,
    crate::model::control_api::PiConfigGet,
    crate::model::control_api::PiConfigEnsureFile,
    crate::model::control_api::PiModelsSetThinkingLevel,
    crate::model::control_api::PiModelsSaveDefault,
    crate::model::control_api::PiModelsClearDefault,
    crate::model::control_api::PiProvidersDiscoverModels,
    crate::model::control_api::PiProvidersSaveCustom,
    crate::model::control_api::PiProvidersRemoveCustom,
    crate::model::control_api::PiProvidersRefreshAuto,
    crate::model::control_api::PiMcpAddPreset,
    crate::model::control_api::PiMcpSetEnabled,
    crate::model::control_api::PiMcpRemove,
    crate::model::control_api::PiMcpLoginStart,
    crate::model::control_api::PiPluginsRemove,
    crate::model::control_api::PiPluginsImport,
    crate::model::control_api::PiPluginsAdopt,
    crate::model::control_api::PiAuthProviders,
    crate::model::control_api::PiAuthModels,
    crate::model::control_api::PiAuthLogout,
    crate::model::control_api::PiAuthLoginStart,
    crate::model::control_api::PiAuthLoginEvents,
    crate::model::control_api::PiAuthLoginAnswer,
    crate::model::control_api::PiAuthLoginCancel,
    crate::model::control_api::QuotaGet,
    crate::model::control_api::QuotaRefresh,
    crate::model::control_api::QuotaSettingsGet,
    crate::model::control_api::QuotaSetEnabled,
    crate::model::control_api::RemoteConfigGet,
    crate::model::control_api::RemoteConfigSet,
    crate::model::control_api::RemoteStatus,
    crate::model::control_api::RemoteStatusWait,
    crate::model::control_api::DevicePairBegin,
    crate::model::control_api::DevicePair,
    crate::model::control_api::DeviceList,
    crate::model::control_api::DeviceRevoke,
    crate::model::control_api::PushRegister,
    crate::model::control_api::PushUnregister,
    crate::model::control_api::NotifySend,
    crate::model::control_api::AttentionMarkRead,
    crate::model::control_api::NewSessionPinsGet,
    crate::model::control_api::NewSessionPinsToggle,
    crate::model::control_api::TerminalThemePublish,
    crate::client::protocol::desktop::DesktopTypeTextCall,
    crate::model::control_api::SessionLaunchPrepare,
    crate::model::control_api::SessionLoadedSkills,
    crate::model::control_api::AgentMemoryEnsureDir,
    crate::model::control_api::AgentMemoryWatch,
    crate::client::protocol::files::ProjectWatchCall,
    crate::client::protocol::workspace::WorkspaceBrowseDirsCall,
    crate::model::control_api::ConversationCreateForAgent,
    crate::model::control_api::NewSessionCatalog,
    crate::model::control_api::SessionCreate,
    crate::model::control_api::SessionDelete,
    crate::model::control_api::SessionAgentBindings,
    crate::model::control_api::ConversationImage,
    crate::model::control_api::ConversationPromptIndex,
    crate::model::control_api::ConversationPrompt,
    crate::model::control_api::ConversationTurnResult,
    crate::model::control_api::SubagentTranscript,
    crate::model::control_api::ResourceUsage,
    crate::model::control_api::AppCommandList,
    crate::model::control_api::AppCommandRun,
}

/// 不经 `call` 也交给 JS 的运行时数据：宿主函数直接交回的形状，声明里按这些名字引用。
fn other_outputs(generators: &mut Generators) {
    generators
        .output
        .subschema_for::<crate::model::daemon_state::DaemonSessionState>();
    generators
        .output
        .subschema_for::<crate::model::workspace_directory::WorkspaceDirectory>();
}

/// 生成整份声明。
fn generate() -> String {
    let mut generators = Generators::new();
    let mut methods = methods(&mut generators);
    other_outputs(&mut generators);
    let mut input = generators.input.take_definitions(true);
    let output = generators.output.take_definitions(true);

    // 两边都有、形状不同的，收的那份改名。引用了改名类型的也要跟着分开，直到不再变。
    let mut renamed: BTreeSet<String> = input
        .iter()
        .filter(|(name, schema)| output.get(*name).is_some_and(|other| other != *schema))
        .map(|(name, _)| name.clone())
        .collect();
    loop {
        let more: Vec<String> = input
            .iter()
            .filter(|(name, schema)| {
                !renamed.contains(*name)
                    && output.contains_key(*name)
                    && references(schema)
                        .iter()
                        .any(|target| renamed.contains(target))
            })
            .map(|(name, _)| name.clone())
            .collect();
        if more.is_empty() {
            break;
        }
        renamed.extend(more);
    }
    for schema in input.values_mut() {
        rename_references(schema, &renamed);
    }
    for method in &mut methods {
        rename_references(&mut method.params, &renamed);
    }

    let mut definitions: BTreeMap<String, Value> = BTreeMap::new();
    for (name, schema) in output {
        definitions.insert(name, schema);
    }
    for (name, schema) in input {
        let name = if renamed.contains(&name) {
            input_name(&name)
        } else {
            name
        };
        if let Some(existing) = definitions.get(&name) {
            assert_eq!(existing, &schema, "类型 `{name}` 两边形状不同却没分开");
            continue;
        }
        definitions.insert(name, schema);
    }

    let mut out =
        String::from("// 生成的，不要手改：见 crates/smelt-plugin-api/src/js/control_types.rs。\n");
    for (name, schema) in &definitions {
        doc(&mut out, schema, "");
        writeln!(
            out,
            "export type {} = {};",
            identifier(name),
            ty(schema, "")
        )
        .unwrap();
    }
    out.push_str("/** 运行时控制接口的方法（call 的 method）和各自的参数、结果。 */\n");
    out.push_str("export interface ControlMethods {\n");
    for method in &methods {
        writeln!(
            out,
            "  {}: {{ params: {}; result: {} }};",
            literal(&Value::String(method.method.as_str().to_string())),
            ty(&method.params, "  "),
            ty(&method.result, "  ")
        )
        .unwrap();
    }
    out.push_str("}\n");
    out
}

fn input_name(name: &str) -> String {
    format!("{name}Input")
}

/// schema 里直接引用的类型名。
fn references(schema: &Value) -> Vec<String> {
    let mut found = Vec::new();
    visit_references(schema, &mut |target| found.push(target.to_string()));
    found
}

fn visit_references(schema: &Value, found: &mut impl FnMut(&str)) {
    match schema {
        Value::Object(object) => {
            for (key, value) in object {
                match (key.as_str(), value) {
                    ("$ref", Value::String(target)) => found(reference_name(target)),
                    _ => visit_references(value, found),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| visit_references(item, found)),
        _ => {}
    }
}

fn rename_references(schema: &mut Value, renamed: &BTreeSet<String>) {
    match schema {
        Value::Object(object) => {
            for (key, value) in object.iter_mut() {
                match (key.as_str(), &mut *value) {
                    ("$ref", Value::String(target)) => {
                        let name = reference_name(target);
                        if renamed.contains(name) {
                            *target = format!("#/$defs/{}", input_name(name));
                        }
                    }
                    _ => rename_references(value, renamed),
                }
            }
        }
        Value::Array(items) => items
            .iter_mut()
            .for_each(|item| rename_references(item, renamed)),
        _ => {}
    }
}

fn reference_name(target: &str) -> &str {
    target
        .strip_prefix("#/$defs/")
        .unwrap_or_else(|| panic!("不认识的引用 `{target}`"))
}

/// 类型名里不能进 TypeScript 标识符的字符换成 `_`。
fn identifier(name: &str) -> String {
    name.chars()
        .map(|it| {
            if it.is_ascii_alphanumeric() || it == '_' {
                it
            } else {
                '_'
            }
        })
        .collect()
}

/// schema 的 `description` 写成 `/** */`，接在 `indent` 缩进后面。
fn doc(out: &mut String, schema: &Value, indent: &str) {
    let Some(text) = schema.get("description").and_then(Value::as_str) else {
        return;
    };
    let text = text.trim().replace("*/", "*\\/");
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() == 1 {
        writeln!(out, "{indent}/** {} */", lines[0]).unwrap();
        return;
    }
    writeln!(out, "{indent}/**").unwrap();
    for line in lines {
        let line = line.trim_end();
        if line.is_empty() {
            writeln!(out, "{indent} *").unwrap();
        } else {
            writeln!(out, "{indent} * {line}").unwrap();
        }
    }
    writeln!(out, "{indent} */").unwrap();
}

fn literal(value: &Value) -> String {
    serde_json::to_string(value).unwrap()
}

/// 并集、交集放进数组或别的交集前加括号。
fn wrap(ty: String) -> String {
    if ty.contains(" | ") || ty.contains(" & ") {
        format!("({ty})")
    } else {
        ty
    }
}

fn union(mut parts: Vec<String>) -> String {
    let mut seen = BTreeSet::new();
    parts.retain(|part| seen.insert(part.clone()));
    match parts.len() {
        0 => "never".to_string(),
        1 => parts.pop().unwrap(),
        _ => parts.join(" | "),
    }
}

/// 一个 schema 的 TypeScript 类型。`indent` 是它所在那行的缩进，展开的对象按它对齐。
fn ty(schema: &Value, indent: &str) -> String {
    let object = match schema {
        Value::Bool(true) => return "unknown".to_string(),
        Value::Bool(false) => return "never".to_string(),
        Value::Object(object) => object,
        other => panic!("不是 schema：{other}"),
    };
    if let Some(value) = object.get("const") {
        return literal(value);
    }
    let mut parts = Vec::new();
    if let Some(Value::String(target)) = object.get("$ref") {
        parts.push(identifier(reference_name(target)));
    }
    if let Some(Value::Array(values)) = object.get("enum") {
        parts.push(union(values.iter().map(literal).collect()));
    } else if let Some(kind) = object.get("type") {
        let kinds: Vec<&str> = match kind {
            Value::String(kind) => vec![kind.as_str()],
            Value::Array(kinds) => kinds.iter().filter_map(Value::as_str).collect(),
            other => panic!("不认识的 type：{other}"),
        };
        parts.push(union(
            kinds
                .into_iter()
                .map(|kind| single(kind, object, indent))
                .collect(),
        ));
    } else if object.contains_key("properties") {
        parts.push(properties(object, indent));
    }
    for key in ["oneOf", "anyOf"] {
        if let Some(Value::Array(choices)) = object.get(key) {
            parts.push(union(
                choices.iter().map(|choice| ty(choice, indent)).collect(),
            ));
        }
    }
    if let Some(Value::Array(all)) = object.get("allOf") {
        parts.extend(all.iter().map(|part| ty(part, indent)));
    }
    match parts.len() {
        0 => "unknown".to_string(),
        1 => parts.pop().unwrap(),
        _ => parts.into_iter().map(wrap).collect::<Vec<_>>().join(" & "),
    }
}

fn single(kind: &str, object: &Map<String, Value>, indent: &str) -> String {
    match kind {
        "string" => "string".to_string(),
        "integer" | "number" => "number".to_string(),
        "boolean" => "boolean".to_string(),
        "null" => "null".to_string(),
        "array" => {
            if let Some(Value::Array(items)) = object.get("prefixItems") {
                let items: Vec<String> = items.iter().map(|item| ty(item, indent)).collect();
                format!("[{}]", items.join(", "))
            } else {
                let item = object
                    .get("items")
                    .map_or("unknown".to_string(), |item| ty(item, indent));
                format!("{}[]", wrap(item))
            }
        }
        "object" => properties(object, indent),
        other => panic!("不认识的 type `{other}`"),
    }
}

/// 对象：列出的字段，加上别的键（`additionalProperties`）。
fn properties(object: &Map<String, Value>, indent: &str) -> String {
    let rest = match object.get("additionalProperties") {
        None | Some(Value::Bool(true)) => Some("unknown".to_string()),
        Some(Value::Bool(false)) => None,
        Some(schema) => Some(ty(schema, indent)),
    };
    let fields = object.get("properties").and_then(Value::as_object);
    let Some(fields) = fields.filter(|fields| !fields.is_empty()) else {
        if object.get("maxProperties").and_then(Value::as_u64) == Some(0) {
            return "Record<string, never>".to_string();
        }
        return format!(
            "{{ [key: string]: {} }}",
            rest.unwrap_or("never".to_string())
        );
    };
    let required: BTreeSet<&str> = object
        .get("required")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let inner = format!("{indent}  ");
    let mut out = String::from("{\n");
    for (name, schema) in fields {
        doc(&mut out, schema, &inner);
        let key = if !name.is_empty()
            && !name.starts_with(|it: char| it.is_ascii_digit())
            && name
                .chars()
                .all(|it| it.is_ascii_alphanumeric() || it == '_' || it == '$')
        {
            name.clone()
        } else {
            literal(&Value::String(name.clone()))
        };
        let optional = if required.contains(name.as_str()) {
            ""
        } else {
            "?"
        };
        writeln!(out, "{inner}{key}{optional}: {};", ty(schema, &inner)).unwrap();
    }
    write!(out, "{indent}}}").unwrap();
    // 列了字段又收别的键的（flatten 了一张表），只在明说了别的键是什么时加上。
    match object.get("additionalProperties") {
        Some(schema) if !matches!(schema, Value::Bool(_)) => {
            format!("{out} & {{ [key: string]: {} }}", rest.unwrap())
        }
        _ => out,
    }
}

mod tests {
    use super::*;

    #[test]
    fn every_control_method_is_declared() {
        let mut generators = Generators::new();
        let listed: Vec<ControlMethod> = methods(&mut generators)
            .iter()
            .map(|method| method.method)
            .collect();
        assert_eq!(
            listed,
            ControlMethod::ALL,
            "control_calls! 要按 ControlMethod 的次序列全"
        );
    }

    #[test]
    fn declarations_are_up_to_date() {
        let generated = generate();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(FILE);
        if std::env::var_os("SMELT_UPDATE_TYPES").is_some() {
            std::fs::write(&path, &generated).unwrap();
            return;
        }
        assert!(
            super::super::CONTROL_TYPES == generated,
            "{FILE} 过期了：线上类型改过。跑 SMELT_UPDATE_TYPES=1 cargo test -j 2 -p smelt-plugin-api \
             --features js --lib js::control_types 重新生成再提交"
        );
    }

    #[test]
    fn generated_names_do_not_shadow_the_hand_written_ones() {
        let names = |text: &str| -> BTreeSet<String> {
            text.lines()
                .filter_map(|line| {
                    let rest = line
                        .strip_prefix("export type ")
                        .or_else(|| line.strip_prefix("export interface "))?;
                    let end = rest.find(|it: char| !it.is_ascii_alphanumeric() && it != '_')?;
                    Some(rest[..end].to_string())
                })
                .collect()
        };
        let clash: Vec<String> = names(super::super::DECLARATIONS)
            .intersection(&names(super::super::CONTROL_TYPES))
            .cloned()
            .collect();
        assert!(clash.is_empty(), "生成的类型和手写的重名：{clash:?}");
    }
}
