//! 本机标题模型的选择：键名和可选模型。存取在运行时（`smelt-core` 的设置目录），
//! GUI 和守护读同一份。

pub const MODEL: &str = "local_title.model";

pub const KEYS: [&str; 1] = [MODEL];

pub fn is_local_title_key(key: &str) -> bool {
    KEYS.contains(&key)
}

/// 本机给会话起标题用的模型。没选过用 [`LocalTitleModel::default`]。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LocalTitleModel {
    #[default]
    Qwen3_5_0_8B,
}

impl LocalTitleModel {
    pub const ALL: [Self; 1] = [Self::Qwen3_5_0_8B];

    /// 设置里存、接口里传的 id。
    pub fn id(self) -> &'static str {
        match self {
            Self::Qwen3_5_0_8B => "qwen3.5-0.8b",
        }
    }

    /// 认不出的 id 返回 `None`，不猜。
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|model| model.id() == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_and_unknown_ids_are_rejected() {
        for model in LocalTitleModel::ALL {
            assert_eq!(LocalTitleModel::from_id(model.id()), Some(model));
        }
        assert_eq!(LocalTitleModel::default(), LocalTitleModel::Qwen3_5_0_8B);
        assert_eq!(LocalTitleModel::default().id(), "qwen3.5-0.8b");
        assert_eq!(LocalTitleModel::from_id("qwen3-4b"), None);
        assert_eq!(LocalTitleModel::from_id(" qwen3.5-0.8b"), None);
        // 已下线：旧选择由存储迁移改成 0.8B，运行时不再认它。
        assert_eq!(LocalTitleModel::from_id("qwen3-0.6b"), None);
    }
}
