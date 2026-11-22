//! 运行时装插件：一个引擎，外壳一包，每个插件单独一包。
//!
//! 插件各自打成一段脚本（[`Compiled`]），作为宿主事件送进跑着外壳的那个引擎，在它自己的作用域里
//! 执行（`plugins/runtime.ts`）。于是：
//! - 装、改、删一个插件只动它自己那几格，外壳里的终端、对话和别的插件不重建；
//! - 一个插件顶层抛错或死循环只坏它自己，冷启动也一样：它不在外壳那一包里；
//! - 宿主知道每次调用、每次装载是哪个插件的（[`CALL`]、[`STATUS`]）。
//!
//! 不用 NSH 的模块热更：所有插件得打成一包，冷启动时一个插件顶层出错整个界面起不来，删掉的
//! 插件顶层登记的东西也撤不掉。
//!
//! 打包和读文件都阻塞，调用方放到后台做；[`apply`] 在主线程。

use std::{
    collections::BTreeMap,
    fs, io,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use nsh_bundler::BundleOptions;
use nsh_core::{access::Capabilities, element::description::SetDescription};
use nsh_engine::Policy;
use nsh_platform_gpui::{App, AppView, Bundle};
use oxc::{
    allocator::Allocator,
    ast::ast::{ImportDeclarationSpecifier, Statement},
    parser::Parser,
    span::SourceType,
};

const APP: &str = include_str!("plugins/App.tsx");
const RUNTIME: &str = include_str!("plugins/runtime.ts");
const SLOTS: &str = include_str!("plugins/slots.tsx");

const INSTALL: &str = "smelt.plugins.install";
const REMOVE: &str = "smelt.plugins.remove";
/// 引擎报装载进度：`{plugin, state: "running" | "ready" | "failed", message}`。顶层死循环接不住，
/// 只有 `running` 没有下文；顶层吃光内存时引擎停掉，最后一个没到 `ready` 的就是它。
pub const STATUS: &str = "smelt.plugins.status";
/// 插件的 `host.call` / `host.post`：`{plugin, name, data}`，`plugin` 由运行时填。
pub const CALL: &str = "smelt.plugins.call";

/// 一个插件的入口文件，导出它的视图。
const ENTRY: &str = "main.tsx";
/// 包里那层函数的参数：宿主模块、交出视图，和按插件记账、所以盖住全局的那几个。
const PARAMS: &str = "__smelt_require, __smelt_provide, setTimeout, setInterval, clearTimeout, \
                      clearInterval, requestAnimationFrame, cancelAnimationFrame";

/// 一个插件打好的脚本。
#[derive(Clone, Debug)]
pub struct Compiled {
    id: String,
    code: Arc<str>,
}

/// 一次改动之后，引擎那头要做的。
#[derive(Clone, Debug)]
pub enum Change {
    /// 起一个新引擎：外壳，加上此刻装着的全部插件。
    Start {
        shell: Arc<str>,
        plugins: Vec<Compiled>,
    },
    /// 装上或者换掉一个插件。
    Install(Compiled),
    /// 卸掉一个插件。
    Remove(String),
}

/// 装着的插件。
pub struct Plugins {
    stage: PathBuf,
    ui_sets: Vec<SetDescription>,
    shell: Arc<str>,
    installed: BTreeMap<String, Compiled>,
}

impl Plugins {
    /// `stage` 是只归它用的目录，打包时摆文件用；`shell` 是导出 `Shell` 的 TSX，插件摆在它画的
    /// `Slot` 里；`ui_sets` 和交给 `set_ui_sets` 的一致。
    pub fn new(stage: &Path, shell: &str, ui_sets: Vec<SetDescription>) -> Result<Self, String> {
        let io = |error: io::Error| format!("{}: {error}", stage.display());
        fs::create_dir_all(stage).map_err(io)?;
        let stage = stage.canonicalize().map_err(io)?;
        let dir = stage.join("shell");
        let natives = natives_module(&ui_sets);
        let files = [
            ("nsh.json", "{}"),
            ("App.tsx", APP),
            ("runtime.ts", RUNTIME),
            ("slots.tsx", SLOTS),
            ("Shell.tsx", shell),
            ("natives.ts", natives.as_str()),
        ];
        write_dir(
            &dir,
            files.map(|(path, text)| (Path::new(path), text.as_bytes())),
        )
        .map_err(io)?;
        let bundle = bundle(&dir.join("App.tsx"), &ui_sets)?;
        Ok(Self {
            shell: bundle.code.into(),
            stage,
            ui_sets,
            installed: BTreeMap::new(),
        })
    }

    /// 起引擎：第一次，或者引擎停了（内存耗尽）之后重来。
    pub fn start(&self) -> Change {
        Change::Start {
            shell: self.shell.clone(),
            plugins: self.installed.values().cloned().collect(),
        }
    }

    /// 装一个插件，或者它的文件变了。打不过包就什么都不变，它上一份照常跑。
    pub fn install(&mut self, id: &str, dir: &Path) -> Result<Change, String> {
        let compiled = self.compile(id, dir)?;
        self.installed.insert(id.to_owned(), compiled.clone());
        Ok(Change::Install(compiled))
    }

    pub fn remove(&mut self, id: &str) -> Change {
        self.installed.remove(id);
        Change::Remove(id.to_owned())
    }

    /// 拷一份到自己的目录里再打包：插件目录里不落生成的文件，import 也出不了这个目录。
    fn compile(&self, id: &str, source: &Path) -> Result<Compiled, String> {
        if !valid_id(id) {
            return Err(format!("插件 id 不能作目录名：{id}"));
        }
        let files = read_dir(source).map_err(|error| format!("{}: {error}", source.display()))?;
        if !files.contains_key(Path::new(ENTRY)) {
            return Err(format!("{id} 没有 {ENTRY}"));
        }
        let dir = self.stage.join("plugins").join(id);
        let entry =
            format!("import * as views from \"./plugin/{ENTRY}\";\n__smelt_provide(views);\n");
        let staged: Vec<(PathBuf, &[u8])> = files
            .iter()
            .map(|(path, bytes)| (Path::new("plugin").join(path), bytes.as_slice()))
            .chain([
                (PathBuf::from("nsh.json"), b"{}".as_slice()),
                (PathBuf::from("entry.ts"), entry.as_bytes()),
            ])
            .collect();
        write_dir(
            &dir,
            staged.iter().map(|(path, bytes)| (path.as_path(), *bytes)),
        )
        .map_err(|error| format!("{}: {error}", dir.display()))?;
        let bundle = bundle(&dir.join("entry.ts"), &self.ui_sets)?;
        if let Some(outside) = bundle.watch_files.iter().find(|file| !inside(file, &dir)) {
            return Err(format!(
                "{id} 引了自己目录以外的文件：{}",
                outside.display()
            ));
        }
        if let Some(asset) = bundle.assets.first() {
            return Err(format!("{id} 引了资源文件，还不支持：{}", asset.display()));
        }
        let served = native_specifiers(&self.ui_sets);
        Ok(Compiled {
            id: id.to_owned(),
            code: function_body(id, &bundle.code, &served)?.into(),
        })
    }
}

fn bundle(entry: &Path, ui_sets: &[SetDescription]) -> Result<nsh_bundler::Bundle, String> {
    nsh_bundler::bundle(&BundleOptions {
        entry: entry.to_path_buf(),
        ui_sets: ui_sets.to_vec(),
        ..Default::default()
    })
    .map_err(|error| error.plain)
}

/// 插件能 import 的宿主模块。外壳一次全引入，插件第一次用到哪个都不必重起引擎。
fn native_specifiers(ui_sets: &[SetDescription]) -> Vec<String> {
    nsh_core::js::NATIVE_MODULES
        .iter()
        .map(|name| (*name).to_owned())
        .chain(ui_sets.iter().map(|set| set.specifier.clone()))
        .collect()
}

fn natives_module(ui_sets: &[SetDescription]) -> String {
    let (mut imports, mut entries) = (String::new(), String::new());
    for (n, name) in native_specifiers(ui_sets).iter().enumerate() {
        imports.push_str(&format!("import * as m{n} from {name:?};\n"));
        entries.push_str(&format!("  {name:?}: m{n},\n"));
    }
    format!("{imports}\nexport const natives: Record<string, any> = {{\n{entries}}};\n")
}

/// 打好的包是一个只剩「import 宿主模块」的 ES 模块；换成一个函数，能在跑着的引擎里执行。
/// import 换成从参数里取，别的原样留着。
fn function_body(id: &str, code: &str, served: &[String]) -> Result<String, String> {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, code, SourceType::mjs()).parse();
    if let Some(error) = parsed.diagnostics.first() {
        return Err(format!("{id}: {error}"));
    }
    let mut body = format!("(function ({PARAMS}) {{\"use strict\";\n");
    let mut at = 0;
    for statement in &parsed.program.body {
        let import = match statement {
            Statement::ImportDeclaration(import) => import,
            Statement::ExportAllDeclaration(_)
            | Statement::ExportDefaultDeclaration(_)
            | Statement::ExportDeclaration(_)
            | Statement::ExportNamedDeclaration(_)
            | Statement::ExportFromDeclaration(_) => {
                return Err(format!("{id}: 包里不该有 export"));
            }
            _ => continue,
        };
        body.push_str(&code[at..import.span.start as usize]);
        at = import.span.end as usize;
        // 按包里剩下的 import 查，不按 `Bundle::natives`：打包时认不得的模块当外部依赖原样留着，
        // 不在那张表里。
        let source = import.source.value.as_str();
        if !served.iter().any(|name| name == source) {
            return Err(format!("{id} 引了宿主没给的模块：{source}"));
        }
        let module = format!("__smelt_require({source:?})");
        let specifiers = import.specifiers.iter().flatten();
        let mut bound = false;
        for specifier in specifiers {
            bound = true;
            let (local, value) = match specifier {
                ImportDeclarationSpecifier::ImportSpecifier(named) => (
                    named.local.name.as_str(),
                    format!("{module}[{:?}]", named.imported.name().as_str()),
                ),
                ImportDeclarationSpecifier::ImportDefaultSpecifier(default) => {
                    (default.local.name.as_str(), format!("{module}.default"))
                }
                ImportDeclarationSpecifier::ImportNamespaceSpecifier(all) => {
                    (all.local.name.as_str(), module.clone())
                }
            };
            body.push_str(&format!("const {local} = {value};"));
        }
        if !bound {
            body.push_str(&format!("{module};"));
        }
    }
    body.push_str(&code[at..]);
    body.push_str("\n})");
    Ok(body)
}

fn valid_id(id: &str) -> bool {
    !id.starts_with('.')
        && !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

/// `file` 在 `dir` 里。两边都是规范过的绝对路径，不含 `..`。
fn inside(file: &Path, dir: &Path) -> bool {
    file.starts_with(dir) && !file.components().any(|part| part == Component::ParentDir)
}

/// 目录里的文件，不跟符号链接，不要点开头的和 `node_modules`。
fn read_dir(root: &Path) -> io::Result<BTreeMap<PathBuf, Vec<u8>>> {
    let mut files = BTreeMap::new();
    let mut dirs = vec![PathBuf::new()];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(root.join(&dir))? {
            let entry = entry?;
            let name = entry.file_name();
            if name.to_string_lossy().starts_with('.') || name == "node_modules" {
                continue;
            }
            let path = dir.join(&name);
            let kind = entry.file_type()?;
            if kind.is_dir() {
                dirs.push(path);
            } else if kind.is_file() {
                files.insert(path, fs::read(entry.path())?);
            }
        }
    }
    Ok(files)
}

/// 把 `dir` 整个换成这些文件。
fn write_dir<'a>(
    dir: &Path,
    files: impl IntoIterator<Item = (&'a Path, &'a [u8])>,
) -> io::Result<()> {
    if dir.exists() {
        fs::remove_dir_all(dir)?;
    }
    for (path, bytes) in files {
        let path = dir.join(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, bytes)?;
    }
    Ok(())
}

/// 把一次改动交给引擎。
pub fn apply(view: &AppView, change: Change) {
    match change {
        Change::Start { shell, plugins } => {
            // 插件脚本用 eval 执行（`plugins/runtime.ts`），所以不开 harden；能力照样检查。
            let mut policy = Policy::development();
            // `nsh/host` 的函数不声明就一个都不给：只声明运行时用的这两个，其它照默认。
            let mut declared = Capabilities::all();
            declared.host = vec![STATUS.to_owned(), CALL.to_owned()];
            policy.declared = Some(declared);
            view.load(App::Js(
                Bundle::source(shell.to_string()).with_policy(policy),
            ));
            for plugin in plugins {
                send_install(view, &plugin);
            }
        }
        Change::Install(plugin) => send_install(view, &plugin),
        Change::Remove(id) => send(view, REMOVE, serde_json::json!({ "plugin": id })),
    }
}

fn send_install(view: &AppView, plugin: &Compiled) {
    send(
        view,
        INSTALL,
        serde_json::json!({ "plugin": plugin.id, "code": &*plugin.code }),
    );
}

fn send(view: &AppView, name: &str, data: serde_json::Value) {
    if let Err(error) = view.host().send(name, data) {
        eprintln!("[nsh-plugins] {name} 送不进去：{error}");
    }
}
