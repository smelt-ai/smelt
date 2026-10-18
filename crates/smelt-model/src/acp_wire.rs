//! 对话快照在长连接上的增量编码。
//!
//! 快照是状态，长连接上原来每一帧都把「从脏的那条起到末尾」整段重发，正在流式增长
//! 的那条消息每长一点就全文重发一遍，传输量随回复长度平方增长。本机 socket 看不出
//! 来，远程链路上会远远落后于生成。
//!
//! 这里让连接两端各持一份 [`FrameMirror`]：发送方的那份记着「对方手里有什么」，接收方
//! 的那份就是手里那份。两边用同一个 [`FrameMirror::apply`] 演进，不会漂移。发送方只发
//! 变化，条目和快照头都一样：按序列化后的 JSON 求公共前缀，只发「保留前几个字节 + 新
//! 的尾巴」。正文、工具输出、子代理记录在哪里增长都是同一个机制，整条替换就是保留 0
//! 字节；快照头逐字段比，没变的字段不发。
//!
//! 线路上只有两种帧：整段帧（附着时的首帧、`Refresh` 之后的帧，形状就是完整的快照帧），
//! 两端用 [`FrameMirror::full`] 重建镜像；增量帧 `{"delta": FrameDelta}`，接收方还原出
//! 完整的帧再照常解析。镜像对不上增量帧时（例如守护交接后接手的读端）这一帧作废，
//! 等下一份整段帧。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::acp_chat::AcpEntry;

/// 一条消息的变化：保留旧 JSON 的前 `keep` 个字节，接上 `tail`。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryPatch {
    pub index: usize,
    pub keep: usize,
    pub tail: String,
}

/// 相对这条连接上一帧的条目变化。`[from, end)` 里没出现在 `patches` 的条目没变；
/// `end` 之后的条目已删除。应用后双方都丢掉 `keep_from` 之前的镜像。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntriesDelta {
    pub from: usize,
    pub end: usize,
    pub keep_from: usize,
    pub patches: Vec<EntryPatch>,
}

/// 镜像跟不上增量帧：缺了它要改的条目。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MirrorGap;

/// 连接一端持有的条目镜像：从 `offset` 起每条消息序列化后的 JSON。
#[derive(Clone, Debug, Default)]
pub struct EntryMirror {
    offset: usize,
    json: Vec<String>,
    /// 还没收到过整段帧，或者对不上过增量帧。此时增量帧一律作废。
    synced: bool,
}

fn entry_json(entry: &AcpEntry) -> String {
    serde_json::to_string(entry).expect("ACP entry contains only serializable data")
}

/// 两段 JSON 的公共前缀，退到字符边界上，保证尾巴本身是合法的字符串。
fn common_prefix(old: &str, new: &str) -> usize {
    let mut keep = old
        .bytes()
        .zip(new.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    while !new.is_char_boundary(keep) {
        keep -= 1;
    }
    keep
}

impl EntryMirror {
    fn end(&self) -> usize {
        self.offset + self.json.len()
    }

    fn get(&self, index: usize) -> Option<&str> {
        index
            .checked_sub(self.offset)
            .and_then(|local| self.json.get(local))
            .map(String::as_str)
    }

    /// 整段帧：从 `offset` 起替换。和视图合并快照的规则一致：落在镜像里就截断接上，
    /// 接不上就整个换成这一段。
    pub fn splice(&mut self, offset: usize, entries: &[AcpEntry]) {
        if self.synced && offset >= self.offset && offset <= self.end() {
            self.json.truncate(offset - self.offset);
        } else {
            self.offset = offset;
            self.json.clear();
        }
        self.json.extend(entries.iter().map(entry_json));
        self.synced = true;
    }

    /// 发送方：`entries` 是从绝对下标 `base` 起的当前条目。算出 `[from, end)` 相对
    /// 镜像的变化，并把它应用到镜像上，镜像从此记着对方收到这一帧之后的样子。
    pub fn encode(
        &mut self,
        base: usize,
        entries: &[AcpEntry],
        from: usize,
        end: usize,
        keep_from: usize,
    ) -> EntriesDelta {
        let end = end.clamp(base, base + entries.len());
        // 对方没有的都算变化：镜像末尾之后的条目即使没被记脏也要发，不能留空洞。
        let from = if self.json.is_empty() {
            from
        } else {
            from.min(self.end())
        }
        .clamp(base, end);
        let patches = (from..end)
            .filter_map(|index| {
                let new = entry_json(&entries[index - base]);
                let keep = match self.get(index) {
                    Some(old) if old == new => return None,
                    Some(old) => common_prefix(old, &new),
                    None => 0,
                };
                Some(EntryPatch {
                    index,
                    keep,
                    tail: new[keep..].to_string(),
                })
            })
            .collect();
        let delta = EntriesDelta {
            from,
            end,
            keep_from,
            patches,
        };
        self.apply(&delta).expect("发送方镜像覆盖自己算出的增量");
        delta
    }

    /// 应用一帧增量，返回 `(第一个变化的下标, 从它到末尾的条目)`，正好是视图按下标
    /// 合并要的那一段。镜像缺了要改的条目就作废，直到下一份整段帧。
    pub fn apply(&mut self, delta: &EntriesDelta) -> Result<(usize, Vec<AcpEntry>), MirrorGap> {
        let result = self.apply_inner(delta);
        if result.is_err() {
            self.synced = false;
        }
        result
    }

    fn apply_inner(&mut self, delta: &EntriesDelta) -> Result<(usize, Vec<AcpEntry>), MirrorGap> {
        if !self.synced || delta.from > delta.end {
            return Err(MirrorGap);
        }
        let start = if self.json.is_empty() {
            delta.from
        } else {
            self.offset.min(delta.from)
        };
        let mut patches = delta.patches.iter().peekable();
        let mut json = Vec::with_capacity(delta.end.saturating_sub(start));
        let mut changed = Vec::new();
        for index in start..delta.end {
            let old = self.get(index);
            let patch = patches.next_if(|patch| patch.index == index);
            let next = match (patch, old) {
                (Some(patch), Some(old))
                    if patch.keep <= old.len() && old.is_char_boundary(patch.keep) =>
                {
                    format!("{}{}", &old[..patch.keep], patch.tail)
                }
                (Some(patch), None) if patch.keep == 0 => patch.tail.clone(),
                (Some(_), _) => return Err(MirrorGap),
                (None, Some(old)) => old.to_string(),
                (None, _) => return Err(MirrorGap),
            };
            if patch.is_some() {
                changed.push(index);
            }
            json.push(next);
        }
        if patches.next().is_some() {
            return Err(MirrorGap);
        }
        let first = changed.first().copied().unwrap_or(delta.end).min(
            // 镜像比这一帧长：末尾被删掉了，视图要从 `end` 截断。
            if self.end() > delta.end {
                delta.end
            } else {
                usize::MAX
            },
        );
        let entries = json[first.saturating_sub(start).min(json.len())..]
            .iter()
            .map(|text| serde_json::from_str(text).map_err(|_| MirrorGap))
            .collect::<Result<Vec<AcpEntry>, _>>()?;
        let keep_from = delta.keep_from.clamp(start, delta.end);
        self.offset = keep_from;
        self.json = json.split_off(keep_from - start);
        Ok((first.min(delta.end), entries))
    }
}

/// 快照头里一个字段的变化：保留旧 JSON 的前 `keep` 个字节，接上 `tail`。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldPatch {
    pub keep: usize,
    pub tail: String,
}

/// 相对这条连接上一帧的变化。`fields` 的键是 `live.phase`、`snapshot.turn_timings`
/// 这样的字段路径，值为 `None` 表示这个字段这一帧没有了。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameDelta {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, Option<FieldPatch>>,
    pub entries: EntriesDelta,
}

/// 帧里的这两个对象按字段拆开比较，其余顶层字段整个比较。
const SPLIT_OBJECTS: [&str; 2] = ["live", "snapshot"];
/// 快照里由条目镜像管的字段，不算快照头。
const ENTRY_FIELDS: [&str; 2] = ["entries", "entries_offset"];

fn header_fields(frame: &Value) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    for (key, value) in frame.as_object().into_iter().flatten() {
        match value.as_object() {
            Some(inner) if SPLIT_OBJECTS.contains(&key.as_str()) => {
                for (field, value) in inner {
                    if key == "snapshot" && ENTRY_FIELDS.contains(&field.as_str()) {
                        continue;
                    }
                    fields.insert(format!("{key}.{field}"), value.to_string());
                }
            }
            _ => {
                fields.insert(key.clone(), value.to_string());
            }
        }
    }
    fields
}

fn frame_from_fields(fields: &BTreeMap<String, String>) -> Result<Map<String, Value>, MirrorGap> {
    let mut frame = Map::new();
    for (key, json) in fields {
        let value: Value = serde_json::from_str(json).map_err(|_| MirrorGap)?;
        match key.split_once('.') {
            Some((outer, field)) => {
                frame
                    .entry(outer)
                    .or_insert_with(|| Value::Object(Map::new()))
                    .as_object_mut()
                    .ok_or(MirrorGap)?
                    .insert(field.to_string(), value);
            }
            None => {
                frame.insert(key.clone(), value);
            }
        }
    }
    Ok(frame)
}

/// 连接一端持有的整帧镜像：快照头各字段的 JSON，加上条目镜像。
#[derive(Clone, Debug, Default)]
pub struct FrameMirror {
    header: BTreeMap<String, String>,
    entries: EntryMirror,
}

impl FrameMirror {
    /// 整段帧：发出或收到时各调一次，镜像变成这一帧的样子。条目读不出来时镜像作废。
    pub fn full(&mut self, frame: &Value) {
        self.header = header_fields(frame);
        let snapshot = &frame["snapshot"];
        let offset = snapshot["entries_offset"].as_u64().unwrap_or(0) as usize;
        match serde_json::from_value::<Vec<AcpEntry>>(snapshot["entries"].clone()) {
            Ok(entries) => self.entries.splice(offset, &entries),
            Err(_) => self.entries.synced = false,
        }
    }

    /// 发送方：`frame` 是这一帧完整的快照头（条目不必带），`entries` 是从绝对下标 `base`
    /// 起的当前条目。算出相对镜像的变化，并应用到镜像上。
    pub fn encode(
        &mut self,
        frame: &Value,
        base: usize,
        entries: &[AcpEntry],
        from: usize,
        end: usize,
        keep_from: usize,
    ) -> FrameDelta {
        let entries = self.encode_entries(base, entries, from, end, keep_from);
        FrameDelta {
            fields: self.encode_header(frame),
            entries,
        }
    }

    /// 发送方第一步：条目的变化。条目要在持有归约状态时读，所以和快照头分开算。
    pub fn encode_entries(
        &mut self,
        base: usize,
        entries: &[AcpEntry],
        from: usize,
        end: usize,
        keep_from: usize,
    ) -> EntriesDelta {
        self.entries.encode(base, entries, from, end, keep_from)
    }

    /// 发送方第二步：快照头里变了的字段。
    pub fn encode_header(&mut self, frame: &Value) -> BTreeMap<String, Option<FieldPatch>> {
        let header = header_fields(frame);
        let mut fields: BTreeMap<String, Option<FieldPatch>> = self
            .header
            .keys()
            .filter(|key| !header.contains_key(*key))
            .map(|key| (key.clone(), None))
            .collect();
        for (key, new) in &header {
            let keep = match self.header.get(key) {
                Some(old) if old == new => continue,
                Some(old) => common_prefix(old, new),
                None => 0,
            };
            fields.insert(
                key.clone(),
                Some(FieldPatch {
                    keep,
                    tail: new[keep..].to_string(),
                }),
            );
        }
        self.header = header;
        fields
    }

    /// 应用一帧增量，还原出完整的快照帧：`snapshot.entries` 是从第一个变化的条目到末尾，
    /// `snapshot.entries_offset` 是它的下标，正是视图按下标合并要的那一段。
    pub fn apply(&mut self, delta: &FrameDelta) -> Result<Value, MirrorGap> {
        let mut header = self.header.clone();
        for (key, patch) in &delta.fields {
            match patch {
                None => {
                    header.remove(key);
                }
                Some(patch) => {
                    let old = header.get(key).map(String::as_str).unwrap_or("");
                    if patch.keep > old.len() || !old.is_char_boundary(patch.keep) {
                        self.entries.synced = false;
                        return Err(MirrorGap);
                    }
                    let new = format!("{}{}", &old[..patch.keep], patch.tail);
                    header.insert(key.clone(), new);
                }
            }
        }
        let mut frame = frame_from_fields(&header)?;
        let (first, entries) = self.entries.apply(&delta.entries)?;
        let snapshot = frame
            .entry("snapshot")
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .ok_or(MirrorGap)?;
        snapshot.insert("entries_offset".into(), first.into());
        snapshot.insert(
            "entries".into(),
            serde_json::to_value(entries).map_err(|_| MirrorGap)?,
        );
        self.header = header;
        Ok(Value::Object(frame))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp_chat::{ToolCallStatus, ToolKind, ToolOutputPart};

    fn assistant(text: &str) -> AcpEntry {
        AcpEntry::Assistant {
            text: text.to_string(),
            thought: false,
        }
    }

    fn texts(entries: &[AcpEntry]) -> Vec<String> {
        entries.iter().map(entry_json).collect()
    }

    /// 发送方和接收方各一份镜像，接收方的结果拼回视图里的条目。
    struct Link {
        sender: EntryMirror,
        receiver: EntryMirror,
        view: Vec<AcpEntry>,
    }

    impl Link {
        fn attach(entries: &[AcpEntry]) -> Self {
            let mut link = Self {
                sender: EntryMirror::default(),
                receiver: EntryMirror::default(),
                view: entries.to_vec(),
            };
            link.sender.splice(0, entries);
            link.receiver.splice(0, entries);
            link
        }

        fn send(&mut self, entries: &[AcpEntry], from: usize, keep_from: usize) -> EntriesDelta {
            let delta = self
                .sender
                .encode(0, entries, from, entries.len(), keep_from);
            let wire: EntriesDelta =
                serde_json::from_str(&serde_json::to_string(&delta).unwrap()).unwrap();
            let (first, tail) = self.receiver.apply(&wire).unwrap();
            self.view.truncate(first);
            self.view.extend(tail);
            delta
        }
    }

    #[test]
    fn a_growing_reply_sends_only_the_new_text() {
        let mut entries = vec![AcpEntry::User("你好".into()), assistant("第一句")];
        let mut link = Link::attach(&entries);
        entries[1] = assistant("第一句，第二句");
        let delta = link.send(&entries, 1, 0);
        assert_eq!(delta.patches.len(), 1);
        assert!(
            delta.patches[0].tail.starts_with("，第二句"),
            "只发新增的文字：{:?}",
            delta.patches[0].tail
        );
        assert_eq!(texts(&link.view), texts(&entries));
    }

    #[test]
    fn unchanged_entries_before_the_change_are_not_resent() {
        let tool = AcpEntry::tool_call(
            "t1",
            "读文件",
            ToolKind::Read,
            ToolCallStatus::Completed,
            vec![ToolOutputPart::Text("x".repeat(100_000))],
        );
        let mut entries = vec![AcpEntry::User("看看".into()), tool, assistant("好")];
        let mut link = Link::attach(&entries);
        entries[2] = assistant("好的");
        let delta = link.send(&entries, 0, 0);
        let bytes: usize = delta.patches.iter().map(|patch| patch.tail.len()).sum();
        assert!(bytes < 100, "工具输出不能跟着重发，实际 {bytes} 字节");
        assert_eq!(texts(&link.view), texts(&entries));
    }

    #[test]
    fn growth_inside_a_tool_call_is_also_a_short_tail() {
        let tool = |output: &str| {
            AcpEntry::tool_call(
                "t1",
                "跑命令",
                ToolKind::Execute,
                ToolCallStatus::InProgress,
                vec![ToolOutputPart::Terminal {
                    id: "term".into(),
                    output: output.into(),
                    truncated: false,
                    exit_code: None,
                    signal: None,
                }],
            )
        };
        let mut entries = vec![tool(&"line\n".repeat(10_000))];
        let mut link = Link::attach(&entries);
        entries[0] = tool(&format!("{}done\n", "line\n".repeat(10_000)));
        let delta = link.send(&entries, 0, 0);
        assert!(delta.patches[0].tail.len() < 200);
        assert_eq!(texts(&link.view), texts(&entries));
    }

    #[test]
    fn new_replaced_and_removed_entries_round_trip() {
        let mut entries = vec![assistant("a"), assistant("b"), assistant("c")];
        let mut link = Link::attach(&entries);
        entries[1] = AcpEntry::Divider("换了".into());
        entries.push(assistant("d"));
        link.send(&entries, 1, 0);
        assert_eq!(texts(&link.view), texts(&entries));
        entries.truncate(2);
        link.send(&entries, 2, 0);
        assert_eq!(texts(&link.view), texts(&entries));
    }

    #[test]
    fn multibyte_text_never_splits_a_character() {
        let mut entries = vec![assistant("你")];
        let mut link = Link::attach(&entries);
        // 「你」和「伱」的 UTF-8 前两个字节相同，按字节的公共前缀会落在字符中间。
        entries[0] = assistant("伱");
        link.send(&entries, 0, 0);
        assert_eq!(texts(&link.view), texts(&entries));
    }

    #[test]
    fn trimmed_entries_come_back_whole_when_they_change_again() {
        let mut entries = vec![assistant("旧回合"), assistant("新回合")];
        let mut link = Link::attach(&entries);
        // 旧回合已经收尾，双方丢掉它的镜像。
        entries[1] = assistant("新回合继续");
        link.send(&entries, 1, 1);
        // 收尾时又改了旧回合那条：镜像里没有，只能整条发。
        entries[0] = assistant("旧回合（已改）");
        let delta = link.send(&entries, 0, 1);
        assert_eq!(delta.patches[0].keep, 0);
        assert_eq!(texts(&link.view), texts(&entries));
    }

    #[test]
    fn a_receiver_without_the_base_waits_for_a_full_frame() {
        let mut entries = vec![assistant("a")];
        let mut sender = EntryMirror::default();
        sender.splice(0, &entries);
        entries[0] = assistant("ab");
        let delta = sender.encode(0, &entries, 0, 1, 0);

        let mut receiver = EntryMirror::default();
        assert_eq!(
            receiver.apply(&delta),
            Err(MirrorGap),
            "没收到过整段帧，增量要作废"
        );
        receiver.splice(0, &entries);
        entries[0] = assistant("abc");
        let delta = sender.encode(0, &entries, 0, 1, 0);
        let (_, tail) = receiver.apply(&delta).unwrap();
        assert_eq!(texts(&tail), texts(&entries));
    }

    #[test]
    fn total_bytes_grow_linearly_with_a_streamed_reply() {
        let mut entries = vec![assistant("")];
        let mut link = Link::attach(&entries);
        let mut sent = 0;
        let mut text = String::new();
        for i in 0..1000 {
            text.push_str(&format!("[{i:04}] 一段流式输出\n"));
            entries[0] = assistant(&text);
            sent += link
                .send(&entries, 0, 0)
                .patches
                .iter()
                .map(|patch| patch.tail.len())
                .sum::<usize>();
        }
        let reply = entry_json(&entries[0]).len();
        assert!(
            sent < reply * 2,
            "流式回复总共发 {sent} 字节，回复本身 {reply} 字节"
        );
        assert_eq!(texts(&link.view), texts(&entries));
    }

    fn frame(revision: u64, phase: &str, entries: &[AcpEntry]) -> Value {
        serde_json::json!({
            "live": {"phase": phase, "history_len": 0},
            "snapshot": {
                "entries_offset": 0,
                "entries_total": entries.len(),
                "snapshot_revision": revision,
                "phase": phase,
                "available_commands": (0..50).map(|i| format!("/command-{i}")).collect::<Vec<_>>(),
                "entries": entries,
            },
            "provider_pid": 42,
        })
    }

    /// 发送方按 `frame` 编码，接收方还原，返回（线路上的字节数，还原出的帧）。
    fn round_trip(
        sender: &mut FrameMirror,
        receiver: &mut FrameMirror,
        frame: &Value,
    ) -> (usize, Value) {
        let entries: Vec<AcpEntry> =
            serde_json::from_value(frame["snapshot"]["entries"].clone()).unwrap();
        let delta = sender.encode(frame, 0, &entries, 0, entries.len(), 0);
        let wire = serde_json::to_string(&serde_json::json!({ "delta": delta })).unwrap();
        let decoded: FrameDelta =
            serde_json::from_value(serde_json::from_str::<Value>(&wire).unwrap()["delta"].clone())
                .unwrap();
        (wire.len(), receiver.apply(&decoded).unwrap())
    }

    fn attached(frame: &Value) -> (FrameMirror, FrameMirror) {
        let mut sender = FrameMirror::default();
        let mut receiver = FrameMirror::default();
        sender.full(frame);
        receiver.full(frame);
        (sender, receiver)
    }

    #[test]
    fn unchanged_header_fields_are_not_resent() {
        let entries = vec![assistant("一段回复")];
        let first = frame(1, "thinking", &entries);
        let (mut sender, mut receiver) = attached(&first);
        let mut entries = entries;
        entries[0] = assistant("一段回复，接着说");
        let next = frame(2, "thinking", &entries);
        let (bytes, restored) = round_trip(&mut sender, &mut receiver, &next);
        assert!(bytes < 200, "只该发修订号和新增文字，实际 {bytes} 字节");
        assert_eq!(restored["snapshot"]["snapshot_revision"], 2);
        assert_eq!(
            restored["snapshot"]["available_commands"],
            next["snapshot"]["available_commands"]
        );
        assert_eq!(restored["live"], next["live"]);
        assert_eq!(restored["provider_pid"], 42);
        assert_eq!(restored["snapshot"]["entries"], next["snapshot"]["entries"]);
    }

    #[test]
    fn changed_and_removed_header_fields_round_trip() {
        let first = frame(1, "thinking", &[]);
        let (mut sender, mut receiver) = attached(&first);
        let mut next = frame(2, "idle", &[]);
        next.as_object_mut().unwrap().remove("provider_pid");
        next["snapshot"]["status_line"] = "完成".into();
        let (_, restored) = round_trip(&mut sender, &mut receiver, &next);
        assert_eq!(restored["live"]["phase"], "idle");
        assert_eq!(restored["snapshot"]["status_line"], "完成");
        assert!(restored.get("provider_pid").is_none());
    }

    #[test]
    fn a_receiver_without_a_full_frame_rejects_deltas() {
        let first = frame(1, "thinking", &[assistant("a")]);
        let mut sender = FrameMirror::default();
        sender.full(&first);
        let entries = vec![assistant("ab")];
        let delta = sender.encode(&frame(2, "thinking", &entries), 0, &entries, 0, 1, 0);
        let mut receiver = FrameMirror::default();
        assert_eq!(receiver.apply(&delta), Err(MirrorGap));
    }
}
