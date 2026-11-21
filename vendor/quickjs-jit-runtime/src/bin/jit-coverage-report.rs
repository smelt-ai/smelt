use rquickjs_jit::{
    abi::OPCODE_FINGERPRINT,
    bytecode::{linked_opcode_table, tier1_policy, Tier1Policy},
};
use serde::Serialize;

#[derive(Serialize)]
struct Coverage {
    schema_version: u32,
    opcode_fingerprint: u64,
    total_opcodes: usize,
    native: usize,
    helper: usize,
    rejected: usize,
    advertised: usize,
}

fn main() {
    let mut native = 0;
    let mut helper = 0;
    let mut rejected = 0;
    for opcode in linked_opcode_table() {
        match tier1_policy(opcode.id()) {
            Some(Tier1Policy::Native) => native += 1,
            Some(Tier1Policy::Helper(_)) => helper += 1,
            Some(Tier1Policy::Reject(_)) | None => rejected += 1,
        }
    }
    let report = Coverage {
        schema_version: 1,
        opcode_fingerprint: OPCODE_FINGERPRINT,
        total_opcodes: native + helper + rejected,
        native,
        helper,
        rejected,
        advertised: native + helper,
    };
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
}
