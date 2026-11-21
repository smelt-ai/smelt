use rquickjs_jit::platform::{CodeAllocator, CodeMemoryError};

fn main() {
    match CodeAllocator::for_host() {
        Ok(_) => {}
        Err(CodeMemoryError::UnsupportedWriteProtection { .. }) => std::process::exit(77),
        Err(error) => {
            eprintln!("host JIT capability probe failed: {error}");
            std::process::exit(1);
        }
    }
}
