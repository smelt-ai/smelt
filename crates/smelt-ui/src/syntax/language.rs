//! 语言表：名字和别名找到语法包和查询。查询第一次用到时才编译，进程里每种语言只编一次。

use std::sync::{Arc, LazyLock, OnceLock};

use tree_sitter::{Language, Query};
use tree_sitter_language::LanguageFn;

/// 一种语言编好的东西：语法、高亮查询、注入查询。
pub(crate) struct Grammar {
    pub(crate) name: &'static str,
    pub(crate) language: Language,
    pub(crate) highlights: Query,
    pub(crate) injections: Option<Injections>,
}

/// 注入查询：哪些节点里是另一种语言的代码（Markdown 围栏、HTML 里的脚本……）。
pub(crate) struct Injections {
    pub(crate) query: Query,
    /// `@injection.content`：要按别的语言解析的节点。
    pub(crate) content: Option<u32>,
    /// `@injection.language`：节点文本就是语言名（围栏的信息串）。
    pub(crate) language: Option<u32>,
    /// 按模式下标排，每个模式上 `#set!` 的设置。
    pub(crate) patterns: Vec<InjectionPattern>,
}

#[derive(Default)]
pub(crate) struct InjectionPattern {
    /// `#set! injection.language "x"` 写死的语言。
    pub(crate) language: Option<Box<str>>,
    /// `#set! injection.combined`：所有匹配并成一份文本一起解析（Markdown 的行内内容）。
    pub(crate) combined: bool,
    /// `#set! injection.include-children`：子节点也算内容；不写时要挖掉子节点。
    pub(crate) include_children: bool,
}

struct Spec {
    name: &'static str,
    aliases: &'static [&'static str],
    language: LanguageFn,
    /// 按顺序拼成一份查询；同一个节点被几条模式命中时，后面的算数。
    highlights: &'static [&'static str],
    injections: &'static str,
}

macro_rules! query {
    ($path:literal) => {
        include_str!(concat!("queries/", $path))
    };
}

/// 名字小写；别名只在这里写一次。`queries/` 下的是改过的查询，其余用语法包自带的。
static SPECS: &[Spec] = &[
    Spec {
        name: "astro",
        aliases: &[],
        language: tree_sitter_astro_next::LANGUAGE,
        highlights: &[tree_sitter_astro_next::HIGHLIGHTS_QUERY],
        injections: tree_sitter_astro_next::INJECTIONS_QUERY,
    },
    Spec {
        name: "bash",
        aliases: &["sh", "shell", "zsh"],
        language: tree_sitter_bash::LANGUAGE,
        highlights: &[tree_sitter_bash::HIGHLIGHT_QUERY],
        injections: "",
    },
    Spec {
        name: "c",
        aliases: &["h"],
        language: tree_sitter_c::LANGUAGE,
        highlights: &[tree_sitter_c::HIGHLIGHT_QUERY],
        injections: "",
    },
    Spec {
        name: "cmake",
        aliases: &[],
        language: tree_sitter_cmake::LANGUAGE,
        highlights: &[tree_sitter_cmake::HIGHLIGHTS_QUERY],
        injections: "",
    },
    // C++ 的查询只写了比 C 多的部分，按语法包的配置先拼 C 的。
    Spec {
        name: "cpp",
        aliases: &["c++", "cc", "cxx", "hpp"],
        language: tree_sitter_cpp::LANGUAGE,
        highlights: &[
            tree_sitter_c::HIGHLIGHT_QUERY,
            tree_sitter_cpp::HIGHLIGHT_QUERY,
        ],
        injections: "",
    },
    Spec {
        name: "csharp",
        aliases: &["cs", "c#"],
        language: tree_sitter_c_sharp::LANGUAGE,
        highlights: &[tree_sitter_c_sharp::HIGHLIGHTS_QUERY],
        injections: "",
    },
    Spec {
        name: "css",
        aliases: &["scss"],
        language: tree_sitter_css::LANGUAGE,
        highlights: &[tree_sitter_css::HIGHLIGHTS_QUERY],
        injections: "",
    },
    Spec {
        name: "diff",
        aliases: &["patch"],
        language: tree_sitter_diff::LANGUAGE,
        highlights: &[tree_sitter_diff::HIGHLIGHTS_QUERY],
        injections: "",
    },
    Spec {
        name: "ejs",
        aliases: &[],
        language: tree_sitter_embedded_template::LANGUAGE,
        highlights: &[tree_sitter_embedded_template::HIGHLIGHTS_QUERY],
        injections: tree_sitter_embedded_template::INJECTIONS_EJS_QUERY,
    },
    Spec {
        name: "elixir",
        aliases: &["ex", "exs"],
        language: tree_sitter_elixir::LANGUAGE,
        highlights: &[tree_sitter_elixir::HIGHLIGHTS_QUERY],
        injections: tree_sitter_elixir::INJECTIONS_QUERY,
    },
    Spec {
        name: "erb",
        aliases: &[],
        language: tree_sitter_embedded_template::LANGUAGE,
        highlights: &[tree_sitter_embedded_template::HIGHLIGHTS_QUERY],
        injections: tree_sitter_embedded_template::INJECTIONS_ERB_QUERY,
    },
    Spec {
        name: "go",
        aliases: &["golang"],
        language: tree_sitter_go::LANGUAGE,
        highlights: &[query!("go/highlights.scm")],
        injections: "",
    },
    Spec {
        name: "html",
        aliases: &["htm", "xhtml"],
        language: tree_sitter_html::LANGUAGE,
        highlights: &[query!("html/highlights.scm")],
        injections: query!("html/injections.scm"),
    },
    Spec {
        name: "java",
        aliases: &[],
        language: tree_sitter_java::LANGUAGE,
        highlights: &[tree_sitter_java::HIGHLIGHTS_QUERY],
        injections: "",
    },
    Spec {
        name: "javascript",
        aliases: &["js", "jsx", "mjs", "cjs"],
        language: tree_sitter_javascript::LANGUAGE,
        highlights: &[
            query!("javascript/highlights.scm"),
            tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
        ],
        injections: query!("javascript/injections.scm"),
    },
    Spec {
        name: "jsdoc",
        aliases: &[],
        language: tree_sitter_jsdoc::LANGUAGE,
        highlights: &[tree_sitter_jsdoc::HIGHLIGHTS_QUERY],
        injections: "",
    },
    Spec {
        name: "json",
        aliases: &["jsonc", "json5"],
        language: tree_sitter_json::LANGUAGE,
        highlights: &[query!("json/highlights.scm")],
        injections: "",
    },
    Spec {
        name: "kotlin",
        aliases: &["kt", "kts", "ktm"],
        language: tree_sitter_kotlin_sg::LANGUAGE,
        highlights: &[query!("kotlin/highlights.scm")],
        injections: "",
    },
    Spec {
        name: "lua",
        aliases: &[],
        language: tree_sitter_lua::LANGUAGE,
        highlights: &[query!("lua/highlights.scm")],
        injections: tree_sitter_lua::INJECTIONS_QUERY,
    },
    Spec {
        name: "make",
        aliases: &["makefile", "mk"],
        language: tree_sitter_make::LANGUAGE,
        highlights: &[tree_sitter_make::HIGHLIGHTS_QUERY],
        injections: "",
    },
    Spec {
        name: "markdown",
        aliases: &["md", "mdx"],
        language: tree_sitter_md::LANGUAGE,
        highlights: &[query!("markdown/highlights.scm")],
        injections: query!("markdown/injections.scm"),
    },
    Spec {
        name: "markdown_inline",
        aliases: &["markdown-inline"],
        language: tree_sitter_md::INLINE_LANGUAGE,
        highlights: &[query!("markdown_inline/highlights.scm")],
        injections: "",
    },
    Spec {
        name: "php",
        aliases: &["php3", "php4", "php5", "phtml"],
        language: tree_sitter_php::LANGUAGE_PHP,
        highlights: &[tree_sitter_php::HIGHLIGHTS_QUERY],
        injections: query!("php/injections.scm"),
    },
    Spec {
        name: "python",
        aliases: &["py", "pyi"],
        language: tree_sitter_python::LANGUAGE,
        highlights: &[tree_sitter_python::HIGHLIGHTS_QUERY],
        injections: "",
    },
    Spec {
        name: "ruby",
        aliases: &["rb"],
        language: tree_sitter_ruby::LANGUAGE,
        highlights: &[tree_sitter_ruby::HIGHLIGHTS_QUERY],
        injections: "",
    },
    Spec {
        name: "rust",
        aliases: &["rs"],
        language: tree_sitter_rust::LANGUAGE,
        highlights: &[query!("rust/highlights.scm")],
        injections: query!("rust/injections.scm"),
    },
    Spec {
        name: "scala",
        aliases: &["sc"],
        language: tree_sitter_scala::LANGUAGE,
        highlights: &[tree_sitter_scala::HIGHLIGHTS_QUERY],
        injections: "",
    },
    Spec {
        name: "sql",
        aliases: &[],
        language: tree_sitter_sequel::LANGUAGE,
        highlights: &[tree_sitter_sequel::HIGHLIGHTS_QUERY],
        injections: "",
    },
    Spec {
        name: "svelte",
        aliases: &[],
        language: tree_sitter_svelte_next::LANGUAGE,
        highlights: &[tree_sitter_svelte_next::HIGHLIGHTS_QUERY],
        injections: tree_sitter_svelte_next::INJECTIONS_QUERY,
    },
    Spec {
        name: "swift",
        aliases: &[],
        language: tree_sitter_swift::LANGUAGE,
        highlights: &[tree_sitter_swift::HIGHLIGHTS_QUERY],
        injections: "",
    },
    Spec {
        name: "toml",
        aliases: &[],
        language: tree_sitter_toml_ng::LANGUAGE,
        highlights: &[tree_sitter_toml_ng::HIGHLIGHTS_QUERY],
        injections: "",
    },
    // TSX 是 TypeScript 加 JSX：TypeScript 的查询再拼上 JSX 的。
    Spec {
        name: "tsx",
        aliases: &[],
        language: tree_sitter_typescript::LANGUAGE_TSX,
        highlights: &[
            query!("typescript/highlights.scm"),
            tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
        ],
        injections: query!("javascript/injections.scm"),
    },
    Spec {
        name: "typescript",
        aliases: &["ts", "mts", "cts"],
        language: tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
        highlights: &[query!("typescript/highlights.scm")],
        injections: query!("javascript/injections.scm"),
    },
    Spec {
        name: "yaml",
        aliases: &["yml"],
        language: tree_sitter_yaml::LANGUAGE,
        highlights: &[tree_sitter_yaml::HIGHLIGHTS_QUERY],
        injections: "",
    },
    Spec {
        name: "zig",
        aliases: &[],
        language: tree_sitter_zig::LANGUAGE,
        highlights: &[query!("zig/highlights.scm")],
        injections: query!("zig/injections.scm"),
    },
];

/// 每种语言编好的结果，下标和 [`SPECS`] 对应。编不过的记成 `None`，不再重编。
static GRAMMARS: LazyLock<Vec<OnceLock<Option<Arc<Grammar>>>>> =
    LazyLock::new(|| SPECS.iter().map(|_| OnceLock::new()).collect());

fn spec_index(name: &str) -> Option<usize> {
    let name = name.trim().to_ascii_lowercase();
    SPECS
        .iter()
        .position(|spec| spec.name == name || spec.aliases.contains(&name.as_str()))
}

/// 语言名或别名（不分大小写）对应的规范名。不认识的（包括纯文本）返回 `None`。
pub fn language_name(name: &str) -> Option<&'static str> {
    spec_index(name).map(|index| SPECS[index].name)
}

/// 语言名或别名对应的语法。不认识或查询编不过时返回 `None`，当纯文本显示。
pub(crate) fn grammar(name: &str) -> Option<Arc<Grammar>> {
    let index = spec_index(name)?;
    GRAMMARS[index]
        .get_or_init(|| build(&SPECS[index]).ok().map(Arc::new))
        .clone()
}

fn build(spec: &Spec) -> Result<Grammar, tree_sitter::QueryError> {
    let language = Language::new(spec.language);
    let highlights = Query::new(&language, &spec.highlights.join("\n"))?;
    let injections = if spec.injections.is_empty() {
        None
    } else {
        Some(injections(Query::new(&language, spec.injections)?))
    };
    Ok(Grammar {
        name: spec.name,
        language,
        highlights,
        injections,
    })
}

fn injections(query: Query) -> Injections {
    let capture = |name: &str| query.capture_index_for_name(name);
    let content = capture("injection.content");
    let language = capture("injection.language");
    let patterns = (0..query.pattern_count())
        .map(|index| {
            let mut pattern = InjectionPattern::default();
            for property in query.property_settings(index) {
                match property.key.as_ref() {
                    "injection.language" => pattern.language = property.value.clone(),
                    "injection.combined" => pattern.combined = true,
                    "injection.include-children" => pattern.include_children = true,
                    _ => {}
                }
            }
            pattern
        })
        .collect();
    Injections {
        query,
        content,
        language,
        patterns,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每种语言的高亮查询和注入查询都编得过：语法包升级改了节点名，这里第一个报。
    #[test]
    fn every_language_query_compiles() {
        let failures: Vec<String> = SPECS
            .iter()
            .filter_map(|spec| build(spec).err().map(|e| format!("{}: {e}", spec.name)))
            .collect();
        assert!(failures.is_empty(), "{failures:#?}");
    }

    #[test]
    fn names_and_aliases_resolve_case_insensitively() {
        assert_eq!(language_name("rust"), Some("rust"));
        assert_eq!(language_name("RS"), Some("rust"));
        assert_eq!(language_name(" ts "), Some("typescript"));
        assert_eq!(language_name("markdown-inline"), Some("markdown_inline"));
        assert_eq!(language_name("text"), None);
        assert_eq!(language_name(""), None);
    }

    /// 名字和别名不能撞：撞了只有表里靠前的那个能找到。
    #[test]
    fn names_and_aliases_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for spec in SPECS {
            for name in std::iter::once(&spec.name).chain(spec.aliases) {
                assert!(seen.insert(*name), "重复的语言名 {name}");
            }
        }
    }
}
