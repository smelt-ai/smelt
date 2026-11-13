//! 桌面进程里跨功能共享的状态。都是 GPUI Global：写入走各模块自己的方法，改完 bump
//! `revision` 唤醒观察者。界面组件不放这里（在 `smelt-ui`）。

pub mod agent_host_state;
pub mod daemon_states_global;
pub mod provider_status_global;
