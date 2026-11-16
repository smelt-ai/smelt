//! 插件之间的约定：一个来源对外给的视图 id、命令、事件、菜单 id 和它们的参数形状。插件
//! 之间不能互相依赖，调用方和提供方都只认这里的定义。一个模块对应一个来源，提供方是那个
//! 来源的插件；提供方没装，命令没登记、视图打不开，调用方据此不给入口
//! （[`crate::command`]、[`crate::is_registered`]）。
//!
//! 改这里就是改两头的约定：同一次改动里把提供方和调用方一起改掉。

pub mod agents;
pub mod capabilities;
pub mod conversation;
pub mod sessions;
pub mod terminal;
