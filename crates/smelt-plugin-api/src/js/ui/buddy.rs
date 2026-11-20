//! `SmeltBuddy.new(key, seed, act, since_ms)`：一个会动的 3D 小人（[`smelt_buddy`]）。
//!
//! 脚本只说是哪个小人（`seed`）、演哪一出、从什么时候起演，演的过程由这里按时间画，脚本不用
//! 每帧重跑；换戏时脚本重画一次。用户看不见（应用、窗口在后台）或减弱动态效果时定格。样式落在
//! 小人那一格（[`smelt_buddy::SIZE`] 见方）外面包的那层上。

use std::sync::Arc;

use gpui::{AnyElement, ElementId, ParentElement as _, Styled as _, div};
use gpui_shell::{
    ArgumentDescriptor, ArgumentSchema, ComponentArgument, ComponentDescriptor,
    ComponentMaterializer, ComponentPayload, ComponentRegistry, ConstructorDescriptor,
    MaterializeRequest, RegistryError, anyhow,
};
use smelt_buddy::{Act, BuddyFigure};
use smelt_ui::motion::{ambient_application_active, ambient_motion_enabled};

use super::finish_leaf;

const OWNER: &str = "SmeltBuddy";

#[derive(Clone)]
struct Buddy {
    key: String,
    seed: String,
    act: Act,
    since_ms: f64,
}

struct BuddyMaterializer;

impl ComponentMaterializer for BuddyMaterializer {
    fn materialize(&self, mut request: MaterializeRequest<'_>) -> anyhow::Result<AnyElement> {
        let Buddy {
            key,
            seed,
            act,
            since_ms,
        } = request
            .payload()
            .downcast_ref::<Buddy>()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("{OWNER} received an incompatible payload"))?;
        let still = request.with_window_app(|window, cx| {
            let ambient = ambient_motion_enabled(
                ambient_application_active(cx),
                window.is_window_active(),
                true,
            );
            Ok(!ambient || cx.reduce_motion())
        })?;
        let figure = BuddyFigure::new(
            ElementId::Name(format!("smelt-buddy:{key}").into()),
            seed,
            act,
            since_ms,
        )
        .still(still);
        finish_leaf(OWNER, request, div().flex().child(figure))
    }
}

/// 写给脚本的说明。演一遍就停的那几出演多久写在这里，脚本据此判断演没演完；和动作的时长、
/// 格子的大小对不上时测试不过。
const DOCUMENTATION: &str = "An animated 3D buddy in a 36 pt square. `seed` decides which buddy (the same seed is always the same one); it plays `act` (\"rest\", \"work\", \"call\", \"cheer\", \"greet\") from `since_ms` (Unix milliseconds) on. A new `act` or `since_ms` starts a new take from the beginning. One-shot acts play once and then stand: \"cheer\" 1.6 s, \"greet\" 1.1 s. The buddy animates by itself, so the script redraws only to change the act; it holds still while the user cannot see it or under reduced motion. `key` follows the buddy's identity. Styles apply to the box around the square.";

pub(super) fn register(registry: &mut ComponentRegistry) -> Result<(), RegistryError> {
    registry.register(
        ComponentDescriptor::new(OWNER, Arc::new(BuddyMaterializer))
            .with_constructors(vec![ConstructorDescriptor::new(
                OWNER,
                vec![
                    ArgumentDescriptor::new("key", ArgumentSchema::String),
                    ArgumentDescriptor::new("seed", ArgumentSchema::String),
                    ArgumentDescriptor::new("act", ArgumentSchema::String),
                    ArgumentDescriptor::new("since_ms", ArgumentSchema::Number),
                ],
                |args| match args {
                    [
                        ComponentArgument::String(key),
                        ComponentArgument::String(seed),
                        ComponentArgument::String(act),
                        ComponentArgument::Number(since_ms),
                    ] if !key.is_empty() && since_ms.is_finite() => {
                        let act = Act::from_name(act).ok_or_else(|| {
                            format!("{OWNER} act must be one of rest, work, call, cheer, greet; got {act:?}")
                        })?;
                        Ok(ComponentPayload::new(Buddy {
                            key: key.clone(),
                            seed: seed.clone(),
                            act,
                            since_ms: *since_ms,
                        }))
                    }
                    _ => Err(format!(
                        "{OWNER}.new(key, seed, act, since_ms) expects a non-empty key, two strings and a finite number"
                    )),
                },
            )])
            .with_documentation(DOCUMENTATION),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 脚本按文档里写的时长判断演没演完，文档要和动作的时长、格子的大小一致。
    #[test]
    fn documentation_matches_the_figure() {
        assert!(DOCUMENTATION.contains(&format!("in a {} pt square", smelt_buddy::SIZE)));
        for act in Act::ALL {
            assert!(DOCUMENTATION.contains(&format!("\"{}\"", act.name())));
            if let Some(length) = act.length() {
                assert!(DOCUMENTATION.contains(&format!("\"{}\" {length} s", act.name())));
            }
        }
    }
}
