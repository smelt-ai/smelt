//! discovery：发现活跃会话并解析人类可读名称，让用户只面对「群名 / 对方姓名」、
//! 永不接触 chatId。移植自 lark_tools 的 feed + 群搜 + 名称解析链路。
//!
//! ⚠️ 高失效风险：cmd 1000 的 f5/f7/f10/f11、cmd 11021 群搜的整套嵌套字段，
//! 都是抓包复制的魔法值，语义不明，飞书前端升级即可能整体失效。已逐处标注。

use super::gateway;
use super::proto::{encode_message, Field, Message, Pb};
use anyhow::Result;
use std::collections::HashSet;

/// 一个对用户可见的会话：只有名称与类型，没有 chatId（chatId 仅内部使用）。
#[derive(Debug, Clone)]
pub struct Conversation {
    pub chat_id: String, // 内部句柄，不展示给用户
    pub name: String,    // 群名 / 对方姓名；p2p 在 pull 阶段才解析，这里可能为空
    pub is_group: bool,
    pub last_msg_time: u64,
}

struct GroupInfo {
    chat_id: String,
    name: String,
    last_msg_time: u64,
}

/// 发现活跃会话：feed 拿种子 → 群搜标出群（带群名）→ 其余种子视为单聊（名称延后）。
pub async fn discover(session: &str) -> Result<Vec<Conversation>> {
    let seeds = fetch_recent_chat_ids(session, 50).await?;
    let groups = search_active_groups(session, &seeds).await?;
    let group_ids: HashSet<String> = groups.iter().map(|g| g.chat_id.clone()).collect();

    let mut convs: Vec<Conversation> = groups
        .into_iter()
        .map(|g| Conversation {
            chat_id: g.chat_id,
            name: g.name,
            is_group: true,
            last_msg_time: g.last_msg_time,
        })
        .collect();

    // 不在群搜结果里的种子 → 单聊。对方姓名要靠消息发送者解析，留到 pull 阶段。
    for id in seeds {
        if !group_ids.contains(&id) {
            convs.push(Conversation {
                chat_id: id,
                name: String::new(),
                is_group: false,
                last_msg_time: 0,
            });
        }
    }
    Ok(convs)
}

/// cmd 1000（Feed Box）：拿最近活跃会话的 chatId 种子。
async fn fetch_recent_chat_ids(session: &str, count: u64) -> Result<Vec<String>> {
    // f1=1(消息) f2=1(向前) f3=0(起始) f4=count(页大小);f5/f7/f10/f11 为魔法值，必须照抄。
    let payload = encode_message(&[
        (1, Pb::Int(1)),
        (2, Pb::Int(1)),
        (3, Pb::Int(0)),
        (4, Pb::Int(count)),
        (5, Pb::Int(0)),  // 魔法值
        (7, Pb::Int(1)),  // 魔法值
        (10, Pb::Int(1)), // 魔法值
        (11, Pb::Int(0)), // 魔法值
    ]);
    let buf = gateway::send(session, 1000, &payload).await?;
    let (_, p) = gateway::decode_response(&buf);
    // 会话列表在 payload.f3[]，每个 f3[].f1 = chatId
    let ids = p
        .get_all(3)
        .iter()
        .filter_map(|f| match f {
            Field::Msg(m) => m.str(1).map(|s| s.to_string()),
            _ => None,
        })
        .collect();
    Ok(ids)
}

/// cmd 11021（SEARCH_CHATS_IN_ADVANCE_SCENE）：用种子排序，翻页拿群名。
async fn search_active_groups(session: &str, seeds: &[String]) -> Result<Vec<GroupInfo>> {
    let session_id = format!("grp_{}", &uuid::Uuid::new_v4().simple().to_string()[..10]);
    let mut token: Option<String> = None;
    let mut out = Vec::new();

    for seq in 1..=30u64 {
        let payload = build_group_search_payload(&session_id, seq, token.as_deref(), seeds);
        let buf = gateway::send(session, 11021, &payload).await?;
        let (packet, p) = gateway::decode_response(&buf);
        if packet.status != 0 {
            break;
        }
        let (groups, next) = parse_groups(&p);
        if groups.is_empty() {
            break;
        }
        out.extend(groups);
        match next {
            Some(t) => token = Some(t),
            None => break,
        }
    }
    Ok(out)
}

/// 构造群搜 payload。整段嵌套字段几乎全是魔法值——改任意一个都可能返回空。
fn build_group_search_payload(
    session_id: &str,
    seq_id: u64,
    token: Option<&str>,
    seeds: &[String],
) -> Vec<u8> {
    // chat_filter = {1:{2:0}, 6:2(按最近降序), 8:1, 10:1, 7:[种子chatId]}
    let mut chat_filter: Vec<(u32, Pb)> = vec![
        (1, Pb::Msg(vec![(2, Pb::Int(0))])),
        (6, Pb::Int(2)),  // 排序：按最近消息降序（魔法值，必须 varint）
        (8, Pb::Int(1)),  // 魔法值
        (10, Pb::Int(1)), // 魔法值
    ];
    for id in seeds {
        chat_filter.push((7, Pb::Str(id.clone()))); // 种子 chatId，repeated；缺它排序不生效
    }

    // scene.f2 是 repeated entityItem：[{1:3, 2:{2:chat_filter}}, {1:24}]
    let scene: Vec<(u32, Pb)> = vec![
        (1, Pb::Str("SEARCH_CHATS_IN_ADVANCE_SCENE".into())),
        (
            2,
            Pb::Msg(vec![
                (1, Pb::Int(3)),
                (2, Pb::Msg(vec![(2, Pb::Msg(chat_filter))])),
            ]),
        ),
        (2, Pb::Msg(vec![(1, Pb::Int(24))])), // 魔法 entityItem
        (3, Pb::Msg(vec![(1, Pb::Int(1)), (8, Pb::Int(1)), (9, Pb::Int(1))])),
        (4, Pb::Msg(vec![(1, Pb::Int(6))])),
        (6, Pb::Msg(vec![(1, Pb::Str(" ".into())), (2, Pb::Int(4))])),
    ];

    let mut search_request: Vec<(u32, Pb)> = vec![
        (1, Pb::Str(session_id.into())),
        (2, Pb::Int(seq_id)),
        (3, Pb::Str(" ".into())), // 查询词留空格
        (5, Pb::Msg(scene)),
        (6, Pb::Str("zh_CN".into())),
        (
            8,
            Pb::Msg(vec![
                (6, Pb::Int(1)),
                (7, Pb::Int(1)),
                (10, Pb::Int(0)),
                (12, Pb::Int(1)),
                (13, Pb::Int(1)),
            ]),
        ), // 整块魔法值
        (9, Pb::Msg(vec![(2, Pb::Int(200))])),  // 魔法值
        (10, Pb::Msg(vec![(2, Pb::Int(202))])), // 魔法值
        (16, Pb::Str("Asia/Shanghai".into())),
        (18, Pb::Str("1".into())), // ⚠️ 字符串 "1"，不是整数
    ];
    if let Some(t) = token {
        search_request.push((4, Pb::Str(t.into()))); // 分页 token
    }

    encode_message(&[(1, Pb::Msg(search_request))])
}

/// 解析群搜响应：payload.f2[] 是群条目，返回 (群列表, 下一页 token)。
fn parse_groups(payload: &Message) -> (Vec<GroupInfo>, Option<String>) {
    let mut groups = Vec::new();
    for item in payload.get_all(2) {
        let Field::Msg(m) = item else { continue };
        let chat_id = m.str(1).unwrap_or("").to_string();
        if chat_id.is_empty() {
            continue;
        }
        let name = strip_highlight(m.str(3).unwrap_or(""));
        // lastMsgTime：f9.f1[] 里 f3=='2' 那项的 f1（秒级时间戳，注意是字符串比较）
        let mut last_msg_time = 0u64;
        if let Some(f9) = m.msg(9) {
            for e in f9.get_all(1) {
                if let Field::Msg(entry) = e {
                    if entry.str(3) == Some("2") {
                        if let Some(t) = entry.str(1).and_then(|s| s.parse().ok()) {
                            last_msg_time = t;
                        }
                    }
                }
            }
        }
        groups.push(GroupInfo { chat_id, name, last_msg_time });
    }

    // 下一页信息：payload.f1.f5 或 payload.f5，是带 HasMore 的 JSON 字符串。
    let next = payload
        .msg(1)
        .and_then(|f1| f1.str(5))
        .or_else(|| payload.str(5))
        .filter(|s| s.starts_with('{') && s.contains("HasMore") && !s.contains("\"HasMore\":false"))
        .map(|s| s.to_string());
    (groups, next)
}

/// cmd 5031：把 userId 解析成姓名。payload 必须是 {1:1(scene), 2:userId}，缺 scene 返回空。
pub async fn resolve_name(session: &str, user_id: &str) -> Option<String> {
    let payload = encode_message(&[(1, Pb::Int(1)), (2, Pb::Str(user_id.into()))]);
    let buf = gateway::send(session, 5031, &payload).await.ok()?;
    let (_, p) = gateway::decode_response(&buf);
    // 姓名在 payload.f2.f2（已是中英拼好的字符串）
    p.msg(2)
        .and_then(|f2| f2.str(2))
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

/// 读取当前登录用户的 userId（~/.lark_cli/user_id），用于在 p2p 消息里区分「对方」。
pub fn load_user_id() -> Option<String> {
    let path = dirs::home_dir()?.join(".lark_cli/user_id");
    let v = std::fs::read_to_string(path).ok()?.trim().to_string();
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

/// 去搜索高亮标签（<h>/<b>/<hb> 等）并还原 &amp;。
fn strip_highlight(s: &str) -> String {
    let pre = s.replace("&amp;", "&");
    let mut out = String::new();
    let mut in_tag = false;
    for c in pre.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::proto::generic_decode;

    #[test]
    fn strip_highlight_removes_search_tags() {
        assert_eq!(strip_highlight("<h>产品</h>群"), "产品群");
        assert_eq!(strip_highlight("A<hb>B</hb>C"), "ABC");
        assert_eq!(strip_highlight("Tom &amp; Jerry"), "Tom & Jerry");
    }

    #[test]
    fn group_search_payload_roundtrips_key_fields() {
        let seeds = vec!["oc_a".to_string(), "oc_b".to_string()];
        let bytes = build_group_search_payload("grp_x", 3, Some("tok"), &seeds);
        // 外层 {1: search_request}
        let outer = generic_decode(&bytes);
        let req = outer.msg(1).expect("f1=search_request");
        assert_eq!(req.str(1), Some("grp_x")); // session_id
        assert_eq!(req.str(2), Some("3")); // seq_id（varint→字符串）
        assert_eq!(req.str(18), Some("1")); // 字符串 "1"
        assert_eq!(req.str(4), Some("tok")); // 分页 token
        let scene = req.msg(5).expect("f5=scene");
        assert_eq!(scene.str(1), Some("SEARCH_CHATS_IN_ADVANCE_SCENE"));
    }

    #[test]
    fn parse_groups_extracts_name_and_time() {
        // 构造 payload.f2 = { f1:"oc_x", f3:"<h>测试</h>群", f9:{ f1:{ f3:"2", f1:"1700" } } }
        let f9 = Pb::Msg(vec![(
            1,
            Pb::Msg(vec![(3, Pb::Str("2".into())), (1, Pb::Str("1700".into()))]),
        )]);
        let item = Pb::Msg(vec![
            (1, Pb::Str("oc_x".into())),
            (3, Pb::Str("<h>测试</h>群".into())),
            (9, f9),
        ]);
        let bytes = encode_message(&[(2, item)]);
        let payload = generic_decode(&bytes);
        let (groups, _) = parse_groups(&payload);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "测试群");
        assert_eq!(groups[0].last_msg_time, 1700);
    }
}
