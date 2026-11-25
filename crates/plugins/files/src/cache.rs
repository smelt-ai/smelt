//! 文件打开内容的有界内存 LRU 缓存：同一台机器的同一个路径在切换文件、后台校验版本未变时
//! 直接命中，减少重复拉取。

use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use smelt_plugin_api::RuntimeKey;
use smelt_plugin_api::model::runtime_path::PathStyle;

/// 缓存总字节上限（32 MB）。超过时按 LRU 淘汰最久未用的条目。
pub(crate) const MAX_CACHE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CachedFileContent {
    Text {
        text: Rc<String>,
        version: String,
    },
    Image {
        bytes: Arc<Vec<u8>>,
        version: Option<String>,
    },
}

impl CachedFileContent {
    fn byte_len(&self) -> usize {
        match self {
            Self::Text { text, .. } => text.len(),
            Self::Image { bytes, .. } => bytes.len(),
        }
    }
}

struct CachedEntry {
    value: CachedFileContent,
    bytes: usize,
}

type CacheKey = (RuntimeKey, String);

pub(crate) struct FileContentCache {
    entries: HashMap<CacheKey, CachedEntry>,
    order: VecDeque<CacheKey>,
    total_bytes: usize,
    max_bytes: usize,
}

impl Default for FileContentCache {
    fn default() -> Self {
        Self::with_max_bytes(MAX_CACHE_BYTES)
    }
}

impl FileContentCache {
    pub(crate) fn with_max_bytes(max_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            total_bytes: 0,
            max_bytes,
        }
    }

    pub(crate) fn get(&mut self, key: &CacheKey) -> Option<CachedFileContent> {
        let value = self.entries.get(key)?.value.clone();
        self.order.retain(|existing| existing != key);
        self.order.push_back(key.clone());
        Some(value)
    }

    pub(crate) fn put(&mut self, key: CacheKey, value: CachedFileContent) {
        self.remove(&key);
        let bytes = value.byte_len();
        if bytes > self.max_bytes / 2 {
            return;
        }
        self.entries
            .insert(key.clone(), CachedEntry { value, bytes });
        self.order.push_back(key);
        self.total_bytes += bytes;
        while self.total_bytes > self.max_bytes {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(removed) = self.entries.remove(&oldest) {
                self.total_bytes -= removed.bytes;
            }
        }
    }

    pub(crate) fn remove(&mut self, key: &CacheKey) {
        if let Some(removed) = self.entries.remove(key) {
            self.total_bytes -= removed.bytes;
            self.order.retain(|existing| existing != key);
        }
    }

    pub(crate) fn remove_under(&mut self, runtime: &RuntimeKey, prefix: &str, style: PathStyle) {
        let victims: Vec<CacheKey> = self
            .entries
            .keys()
            .filter(|(rt, path)| rt == runtime && style.starts_with(path, prefix))
            .cloned()
            .collect();
        for key in victims {
            self.remove(&key);
        }
    }

    #[cfg(test)]
    fn total_bytes(&self) -> usize {
        self.total_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(body: &str, version: &str) -> CachedFileContent {
        CachedFileContent::Text {
            text: Rc::new(body.to_string()),
            version: version.to_string(),
        }
    }

    #[test]
    fn lru_evicts_oldest_and_respects_get_recency() {
        let mut cache = FileContentCache::with_max_bytes(10);
        let a = (RuntimeKey::Local, "/a".to_string());
        let b = (RuntimeKey::Local, "/b".to_string());
        let c = (RuntimeKey::Local, "/c".to_string());

        cache.put(a.clone(), text("1234", "v1"));
        cache.put(b.clone(), text("1234", "v2"));
        assert_eq!(cache.total_bytes(), 8);

        // Touch `a` so `b` becomes least recently used.
        assert!(cache.get(&a).is_some());

        cache.put(c.clone(), text("1234", "v3"));
        assert!(cache.get(&a).is_some());
        assert!(cache.get(&b).is_none());
        assert!(cache.get(&c).is_some());
        assert_eq!(cache.total_bytes(), 8);
    }

    #[test]
    fn overwrite_updates_byte_accounting_and_skips_oversized_entries() {
        let mut cache = FileContentCache::with_max_bytes(10);
        let a = (RuntimeKey::Local, "/a".to_string());

        cache.put(a.clone(), text("12345", "v1"));
        assert_eq!(cache.total_bytes(), 5);

        cache.put(a.clone(), text("12", "v2"));
        assert_eq!(cache.total_bytes(), 2);

        // Single entry > max_bytes / 2 (5) is skipped and removes stale prior entry.
        cache.put(a.clone(), text("123456", "v3"));
        assert!(cache.get(&a).is_none());
        assert_eq!(cache.total_bytes(), 0);
    }

    #[test]
    fn remove_under_drops_subtree_for_matching_runtime_only() {
        let mut cache = FileContentCache::with_max_bytes(1024);
        let r1 = RuntimeKey::Remote("m1".into());
        let r2 = RuntimeKey::Remote("m2".into());

        cache.put((r1.clone(), r"E:\proj\a.md".into()), text("a", "1"));
        cache.put((r1.clone(), r"E:\proj\sub\b.md".into()), text("b", "2"));
        cache.put((r1.clone(), r"E:\project\c.md".into()), text("c", "3"));
        cache.put((r2.clone(), r"E:\proj\a.md".into()), text("d", "4"));

        cache.remove_under(&r1, r"E:\proj", PathStyle::Windows);

        assert!(cache.get(&(r1.clone(), r"E:\proj\a.md".into())).is_none());
        assert!(
            cache
                .get(&(r1.clone(), r"E:\proj\sub\b.md".into()))
                .is_none()
        );
        assert!(cache.get(&(r1, r"E:\project\c.md".into())).is_some());
        assert!(cache.get(&(r2, r"E:\proj\a.md".into())).is_some());
    }
}
