//! 更新插件自己的库（`~/.smelt/plugin-data/smelt.updater/store.sqlite3`）：更新通道和要不要自动
//! 下载安装。每台电脑各管各的更新，所以存在本机、不进运行时。
//!
//! 只在启动时读一次，改了在事件里同步写。库打不开（权限、版本比插件新）就只在内存里用，
//! 日志里留一句。

use std::sync::Arc;

use api::PluginStore;
use api::rusqlite::{OptionalExtension as _, params};
use api::updater::UpdateChannel;
use smelt_plugin_api as api;

use crate::OWNER;

const MIGRATIONS: &[&str] = &["
    CREATE TABLE prefs (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        channel TEXT NOT NULL,
        auto_install INTEGER NOT NULL
    );
"];

/// 更新的偏好。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Prefs {
    /// 更新源给哪一路包：GitHub 上认不认预发布，或打包时指定的哪份清单。
    pub(crate) channel: UpdateChannel,
    /// 发现新版本后要不要自动下载并装好。关掉只停「自动下载」这一步，检查照做：不然既不
    /// 自动更新也看不到有新版，等于彻底静默。
    pub(crate) auto_install: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            channel: UpdateChannel::default(),
            auto_install: true,
        }
    }
}

/// 通道在库里存的名字，和更新清单地址里 `{channel}` 的写法一致。
fn channel_name(channel: UpdateChannel) -> &'static str {
    match channel {
        UpdateChannel::Dev => "dev",
        UpdateChannel::Prod => "prod",
    }
}

/// 认不出的名字（库是别的版本写的）用出厂值。
fn channel_from_name(name: &str) -> UpdateChannel {
    match name {
        "dev" => UpdateChannel::Dev,
        "prod" => UpdateChannel::Prod,
        _ => UpdateChannel::default(),
    }
}

pub(crate) struct UpdaterStore {
    store: Option<Arc<PluginStore>>,
}

impl UpdaterStore {
    /// 打开本插件的库。打不开就是个什么都不记的空壳。
    pub(crate) fn open() -> Self {
        let store = api::plugin_store(OWNER)
            .map_err(|error| error.to_string())
            .and_then(|store| {
                store
                    .migrate(MIGRATIONS)
                    .map_err(|error| error.to_string())
                    .map(|()| store)
            });
        match store {
            Ok(store) => Self { store: Some(store) },
            Err(error) => {
                api::app_log::error("updater", &format!("库打不开，更新设置不会保存：{error}"));
                Self { store: None }
            }
        }
    }

    pub(crate) fn prefs(&self) -> Prefs {
        let Some(store) = &self.store else {
            return Prefs::default();
        };
        let read = store.read(|connection| {
            connection
                .query_row(
                    "SELECT channel, auto_install FROM prefs WHERE id = 1",
                    [],
                    |row| {
                        Ok(Prefs {
                            channel: channel_from_name(&row.get::<_, String>(0)?),
                            auto_install: row.get(1)?,
                        })
                    },
                )
                .optional()
        });
        match read {
            Ok(prefs) => prefs.unwrap_or_default(),
            Err(error) => {
                api::app_log::error("updater", &format!("读更新设置失败，用出厂值：{error}"));
                Prefs::default()
            }
        }
    }

    pub(crate) fn save_prefs(&self, prefs: Prefs) {
        let Some(store) = &self.store else {
            return;
        };
        let written = store.write(|transaction| {
            transaction.execute(
                "INSERT INTO prefs (id, channel, auto_install) VALUES (1, ?1, ?2)
                 ON CONFLICT (id) DO UPDATE SET
                     channel = excluded.channel,
                     auto_install = excluded.auto_install",
                params![channel_name(prefs.channel), prefs.auto_install],
            )
        });
        if let Err(error) = written {
            api::app_log::error("updater", &format!("保存更新设置失败：{error}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_names_roundtrip() {
        for channel in UpdateChannel::ALL {
            assert_eq!(channel_from_name(channel_name(channel)), channel);
        }
        assert_eq!(channel_from_name("nightly"), UpdateChannel::default());
    }
}
