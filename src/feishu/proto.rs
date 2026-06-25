//! proto：无 schema 的 protobuf 编解码，1:1 移植自 lark_tools 的 proto.py。
//!
//! ⚠️ 关键陷阱：字段必须按正确 wire type 编码——整数走 varint(wire 0)、
//! 字符串/字节走 length-delimited(wire 2)。两者混用不会报错，而是**静默失效**
//! (返回空)。例如探活 cmd 84 的 `{1: "0"}` 里 `"0"` 必须是字符串，不能是整数 0。

use std::collections::BTreeMap;

/// 编码用的 protobuf 值。
#[derive(Debug, Clone)]
pub enum Pb {
    /// 整数 → varint(wire type 0)
    Int(u64),
    /// 字符串 → UTF-8 length-delimited(wire type 2)
    Str(String),
    /// 原始字节 → length-delimited(wire type 2)
    Bytes(Vec<u8>),
    /// 嵌套消息 → length-delimited;字段有序，同一字段号出现多次即 repeated。
    Msg(Vec<(u32, Pb)>),
}

// ========== varint ==========

/// 把非负整数编码为 protobuf varint。
pub fn encode_varint(mut n: u64) -> Vec<u8> {
    if n == 0 {
        return vec![0];
    }
    let mut out = Vec::new();
    while n > 0 {
        let mut bits = (n & 0x7f) as u8;
        n >>= 7;
        if n > 0 {
            bits |= 0x80;
        }
        out.push(bits);
    }
    out
}

/// 从 pos 处解一个 varint，返回值并推进 pos。
pub fn decode_varint(data: &[u8], pos: &mut usize) -> u64 {
    let mut result: u64 = 0;
    let mut shift = 0u32;
    while *pos < data.len() {
        let b = data[*pos];
        *pos += 1;
        // 合法 varint 最多 10 字节（shift 至多 63）；畸形数据可能让 shift 越界，
        // 此时高位丢弃而非 panic（对齐 Python 无限精度不崩的行为）。
        if shift < 64 {
            result |= ((b & 0x7f) as u64) << shift;
        }
        if b & 0x80 == 0 {
            break;
        }
        shift += 7;
    }
    result
}

// ========== 编码 ==========

/// 编码字段列表为 protobuf 字节。
pub fn encode_message(fields: &[(u32, Pb)]) -> Vec<u8> {
    let mut buf = Vec::new();
    for (num, val) in fields {
        write_field(&mut buf, *num, val);
    }
    buf
}

fn write_field(buf: &mut Vec<u8>, num: u32, val: &Pb) {
    match val {
        Pb::Int(n) => {
            buf.extend(encode_varint(((num as u64) << 3) | 0));
            buf.extend(encode_varint(*n));
        }
        Pb::Str(s) => {
            let b = s.as_bytes();
            buf.extend(encode_varint(((num as u64) << 3) | 2));
            buf.extend(encode_varint(b.len() as u64));
            buf.extend_from_slice(b);
        }
        Pb::Bytes(b) => {
            buf.extend(encode_varint(((num as u64) << 3) | 2));
            buf.extend(encode_varint(b.len() as u64));
            buf.extend_from_slice(b);
        }
        Pb::Msg(fields) => {
            let nested = encode_message(fields);
            buf.extend(encode_varint(((num as u64) << 3) | 2));
            buf.extend(encode_varint(nested.len() as u64));
            buf.extend(nested);
        }
    }
}

// ========== 通用解码（无 schema）==========

/// 解码出的字段值。
#[derive(Debug, Clone)]
pub enum Field {
    /// wire 0 varint → 字符串（对齐 Python 的 str(int)，避免 i64 精度问题）
    Str(String),
    /// wire 1/5 → 浮点
    F64(f64),
    /// 嵌套消息
    Msg(Message),
    /// 无法解析的二进制，仅记录长度（对齐 Python `<bytes:N>`）
    Opaque(usize),
}

/// 一条解码后的消息：字段号 → 值列表（支持 repeated）。
#[derive(Debug, Clone, Default)]
pub struct Message {
    pub fields: BTreeMap<u32, Vec<Field>>,
}

impl Message {
    /// 取某字段的第一个值。
    pub fn get(&self, f: u32) -> Option<&Field> {
        self.fields.get(&f).and_then(|v| v.first())
    }
    /// 取某字段的全部值（repeated）。
    pub fn get_all(&self, f: u32) -> &[Field] {
        self.fields.get(&f).map(|v| v.as_slice()).unwrap_or(&[])
    }
    /// 取字符串字段。
    pub fn str(&self, f: u32) -> Option<&str> {
        match self.get(f) {
            Some(Field::Str(s)) => Some(s),
            _ => None,
        }
    }
    /// 取嵌套消息字段。
    pub fn msg(&self, f: u32) -> Option<&Message> {
        match self.get(f) {
            Some(Field::Msg(m)) => Some(m),
            _ => None,
        }
    }
}

/// 通用解码：把 protobuf 字节解成 Message 树，无需 schema。
pub fn generic_decode(data: &[u8]) -> Message {
    decode_depth(data, 0)
}

fn decode_depth(data: &[u8], depth: u32) -> Message {
    let mut msg = Message::default();
    if depth > 10 || data.is_empty() {
        return msg;
    }
    let mut pos = 0;
    while pos < data.len() {
        let tag = decode_varint(data, &mut pos);
        let fnum = (tag >> 3) as u32;
        let wt = (tag & 7) as u8;
        let value = match wt {
            0 => {
                let v = decode_varint(data, &mut pos);
                Field::Str(v.to_string()) // 对齐 Python str(value)
            }
            1 => {
                if pos + 8 > data.len() {
                    break;
                }
                let v = f64::from_le_bytes(data[pos..pos + 8].try_into().unwrap());
                pos += 8;
                Field::F64(v)
            }
            2 => {
                let len = decode_varint(data, &mut pos) as usize;
                if pos + len > data.len() {
                    break;
                }
                let raw = &data[pos..pos + len];
                pos += len;
                decode_len_delimited(raw, depth)
            }
            5 => {
                if pos + 4 > data.len() {
                    break;
                }
                let v = f32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as f64;
                pos += 4;
                Field::F64(v)
            }
            _ => break, // 未知 wire type，停止
        };
        msg.fields.entry(fnum).or_default().push(value);
    }
    msg
}

/// wire type 2 的启发式：可打印短串当字符串，否则尝试当嵌套消息，再不行记为字节。
fn decode_len_delimited(raw: &[u8], depth: u32) -> Field {
    let text = std::str::from_utf8(raw).ok();
    let is_printable = match text {
        Some(t) => !t.is_empty() && t.chars().all(is_printable_char),
        None => false,
    };
    if is_printable && raw.len() < 2000 {
        return Field::Str(text.unwrap().to_string());
    }
    let nested = decode_depth(raw, depth + 1);
    if !nested.fields.is_empty() {
        Field::Msg(nested)
    } else if is_printable {
        Field::Str(text.unwrap().to_string())
    } else {
        Field::Opaque(raw.len())
    }
}

/// 对齐 proto.py 的可打印判断：ASCII 可见、CJK、其余 BMP 非控制字符、以及常见空白。
fn is_printable_char(c: char) -> bool {
    matches!(c, ' '..='~')
        || ('\u{4e00}'..='\u{9fff}').contains(&c)
        || ('\u{0080}'..='\u{ffff}').contains(&c)
        || c == '\n'
        || c == '\r'
        || c == '\t'
}

// ========== 原始字段导航（保留二进制）==========

/// 提取首个匹配字段的原始内容（wire 2 返回原始字节）。
pub fn extract_raw_field(data: &[u8], field: u32) -> Option<Vec<u8>> {
    let mut pos = 0;
    while pos < data.len() {
        let tag = decode_varint(data, &mut pos);
        let fnum = (tag >> 3) as u32;
        let wt = (tag & 7) as u8;
        if fnum == field && wt == 2 {
            let len = decode_varint(data, &mut pos) as usize;
            if pos + len > data.len() {
                return None;
            }
            return Some(data[pos..pos + len].to_vec());
        }
        match wt {
            0 => {
                decode_varint(data, &mut pos);
            }
            1 => pos += 8,
            2 => {
                let len = decode_varint(data, &mut pos) as usize;
                pos += len;
            }
            5 => pos += 4,
            _ => break,
        }
    }
    None
}

/// 按路径导航嵌套字段：`extract_raw_path(buf, &[5, 2, 3])` → field5.field2.field3。
pub fn extract_raw_path(data: &[u8], path: &[u32]) -> Option<Vec<u8>> {
    let mut current = data.to_vec();
    for &f in path {
        current = extract_raw_field(&current, f)?;
    }
    Some(current)
}

// ========== Packet（已知 schema）==========
// 字段：1=sid, 2=payload_type, 3=cmd, 4=status, 5=payload(bytes), 6=cid(string)

/// 解码后的外层 Packet。
#[derive(Debug, Clone, Default)]
pub struct Packet {
    pub sid: u64,
    pub payload_type: u64,
    pub cmd: u64,
    pub status: u64,
    pub payload: Vec<u8>,
    pub cid: String,
}

/// 编码外层 Packet。
pub fn encode_packet(payload_type: u64, cmd: u64, payload: &[u8], cid: &str) -> Vec<u8> {
    let mut fields: Vec<(u32, Pb)> = Vec::new();
    if payload_type != 0 {
        fields.push((2, Pb::Int(payload_type)));
    }
    fields.push((3, Pb::Int(cmd)));
    if !payload.is_empty() {
        fields.push((5, Pb::Bytes(payload.to_vec())));
    }
    if !cid.is_empty() {
        fields.push((6, Pb::Str(cid.to_string())));
    }
    encode_message(&fields)
}

/// 解码外层 Packet，payload 保留为原始字节。
pub fn decode_packet(data: &[u8]) -> Packet {
    let mut p = Packet::default();
    let mut pos = 0;
    while pos < data.len() {
        let tag = decode_varint(data, &mut pos);
        let fnum = (tag >> 3) as u32;
        let wt = (tag & 7) as u8;
        match wt {
            0 => {
                let v = decode_varint(data, &mut pos);
                match fnum {
                    1 => p.sid = v,
                    2 => p.payload_type = v,
                    3 => p.cmd = v,
                    4 => p.status = v,
                    _ => {}
                }
            }
            2 => {
                let len = decode_varint(data, &mut pos) as usize;
                if pos + len > data.len() {
                    break;
                }
                let raw = &data[pos..pos + len];
                pos += len;
                match fnum {
                    5 => p.payload = raw.to_vec(),
                    6 => p.cid = String::from_utf8_lossy(raw).into_owned(),
                    _ => {}
                }
            }
            1 => pos += 8,
            5 => pos += 4,
            _ => break,
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_roundtrip() {
        assert_eq!(encode_varint(0), vec![0]);
        assert_eq!(encode_varint(1), vec![1]);
        assert_eq!(encode_varint(300), vec![0xac, 0x02]); // 经典 protobuf 示例
        for n in [0u64, 1, 127, 128, 300, 16384, u32::MAX as u64] {
            let bytes = encode_varint(n);
            let mut pos = 0;
            assert_eq!(decode_varint(&bytes, &mut pos), n);
            assert_eq!(pos, bytes.len());
        }
    }

    #[test]
    fn decode_varint_survives_malformed_overflow() {
        // 畸形：一串永不终止的 continuation 字节，shift 会越过 64——不应 panic。
        let data = vec![0xffu8; 12];
        let mut pos = 0;
        let _ = decode_varint(&data, &mut pos);
        assert_eq!(pos, 12);
    }

    #[test]
    fn string_field_uses_wire_type_2() {
        // cmd 84 探活：{1: "0"} 必须编成字符串，不是整数 0。
        let got = encode_message(&[(1, Pb::Str("0".into()))]);
        // tag = (1<<3)|2 = 0x0a，len=1，'0'=0x30
        assert_eq!(got, vec![0x0a, 0x01, 0x30]);
    }

    #[test]
    fn int_field_uses_wire_type_0() {
        // 对比：{1: 0} 整数应编成 varint，与字符串完全不同。
        let got = encode_message(&[(1, Pb::Int(300))]);
        // tag = (1<<3)|0 = 0x08，varint(300)=[0xac,0x02]
        assert_eq!(got, vec![0x08, 0xac, 0x02]);
    }

    #[test]
    fn packet_roundtrip() {
        let payload = encode_message(&[(1, Pb::Str("hello".into()))]);
        let bytes = encode_packet(1, 8, &payload, "cid-123");
        let p = decode_packet(&bytes);
        assert_eq!(p.payload_type, 1);
        assert_eq!(p.cmd, 8);
        assert_eq!(p.cid, "cid-123");
        assert_eq!(p.payload, payload);
        assert_eq!(p.status, 0);
    }

    #[test]
    fn generic_decode_scalar_and_string() {
        let bytes = encode_message(&[(1, Pb::Int(5)), (2, Pb::Str("hi".into()))]);
        let m = generic_decode(&bytes);
        assert_eq!(m.str(1), Some("5")); // varint → 数字字符串
        assert_eq!(m.str(2), Some("hi"));
    }

    #[test]
    fn generic_decode_nested() {
        let inner = vec![(1, Pb::Str("张三".into()))];
        let bytes = encode_message(&[(3, Pb::Msg(inner))]);
        let m = generic_decode(&bytes);
        let sub = m.msg(3).expect("f3 应是嵌套消息");
        assert_eq!(sub.str(1), Some("张三"));
    }

    #[test]
    fn generic_decode_repeated() {
        let bytes = encode_message(&[(1, Pb::Int(10)), (1, Pb::Int(20)), (1, Pb::Int(30))]);
        let m = generic_decode(&bytes);
        assert_eq!(m.get_all(1).len(), 3);
    }

    #[test]
    fn extract_raw_path_navigates() {
        // 构造 field5 { field2 { field3: "x" } }
        let f3 = encode_message(&[(3, Pb::Str("命中".into()))]);
        let f2 = encode_message(&[(2, Pb::Bytes(f3))]);
        let top = encode_message(&[(5, Pb::Bytes(f2))]);
        let got = extract_raw_path(&top, &[5, 2, 3]).expect("应导航到 f5.f2.f3");
        assert_eq!(String::from_utf8(got).unwrap(), "命中");
    }
}
