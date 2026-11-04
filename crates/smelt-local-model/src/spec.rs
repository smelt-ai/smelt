/// 一个固定版本的模型文件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelSpec {
    /// Hugging Face 仓库。
    pub repo: &'static str,
    /// 固定的仓库提交，保证 URL 内容不随上游变化。
    pub revision: &'static str,
    /// 仓库内文件名，也用作本地文件名。
    pub file_name: &'static str,
    /// 文件字节数。
    pub size: u64,
    /// 文件内容 sha256（小写十六进制）。
    pub sha256: &'static str,
}

impl ModelSpec {
    /// 固定 revision 的直接下载地址（会 302 到 CDN）。
    pub fn url(&self) -> String {
        format!(
            "https://huggingface.co/{}/resolve/{}/{}",
            self.repo, self.revision, self.file_name
        )
    }
}

/// ggml-org GGUF LFS oid 与 size，固定到仓库 revision。
/// 来源：`https://huggingface.co/api/models/ggml-org/Qwen3.5-0.8B-GGUF/tree/807dfd9a6fadbf703207aac3bfb0b2b3040d077c`
pub const QWEN3_5_0_8B_Q8_0: ModelSpec = ModelSpec {
    repo: "ggml-org/Qwen3.5-0.8B-GGUF",
    revision: "807dfd9a6fadbf703207aac3bfb0b2b3040d077c",
    file_name: "Qwen3.5-0.8B-Q8_0.gguf",
    size: 833_592_096,
    sha256: "37ae482d336108d23516fa35e8e0c4126688d81018b87178a18d752a1357814f",
};

pub use smelt_model::local_title_settings::LocalTitleModel as TitleModel;

/// 每个标题模型对应的下载文件和展示名。
pub trait TitleModelSpec {
    fn spec(self) -> &'static ModelSpec;
    fn label(self) -> &'static str;
}

impl TitleModelSpec for TitleModel {
    fn spec(self) -> &'static ModelSpec {
        match self {
            Self::Qwen3_5_0_8B => &QWEN3_5_0_8B_Q8_0,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Qwen3_5_0_8B => "Qwen3.5-0.8B（Q8_0）",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_models_have_distinct_ids_and_files_and_round_trip() {
        for (i, a) in TitleModel::ALL.into_iter().enumerate() {
            for b in &TitleModel::ALL[i + 1..] {
                assert_ne!(a.id(), b.id());
                assert_ne!(a.spec().file_name, b.spec().file_name);
            }
        }
        for model in TitleModel::ALL {
            assert_eq!(TitleModel::from_id(model.id()), Some(model));
            assert_eq!(model.spec().sha256.len(), 64);
        }
        assert_eq!(TitleModel::from_id("unknown"), None);
        assert_eq!(TitleModel::from_id("qwen3-0.6b"), None);
        assert_eq!(TitleModel::default(), TitleModel::Qwen3_5_0_8B);
        assert_eq!(TitleModel::ALL, [TitleModel::Qwen3_5_0_8B]);
    }

    #[test]
    fn url_pins_revision() {
        assert_eq!(
            QWEN3_5_0_8B_Q8_0.url(),
            "https://huggingface.co/ggml-org/Qwen3.5-0.8B-GGUF/resolve/807dfd9a6fadbf703207aac3bfb0b2b3040d077c/Qwen3.5-0.8B-Q8_0.gguf"
        );
        assert_eq!(QWEN3_5_0_8B_Q8_0.size, 833_592_096);
        assert_eq!(QWEN3_5_0_8B_Q8_0.sha256, "37ae482d336108d23516fa35e8e0c4126688d81018b87178a18d752a1357814f");
    }
}
