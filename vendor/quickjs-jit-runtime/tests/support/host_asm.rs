use rquickjs_jit::platform::{CodeMemoryError, WritableCode};

#[cfg(all(target_endian = "little", target_arch = "x86_64"))]
const RETURN_42: &[u8] = &[0xb8, 0x2a, 0x00, 0x00, 0x00, 0xc3];

#[cfg(all(target_endian = "little", target_arch = "aarch64"))]
const RETURN_42: &[u8] = &[0x40, 0x05, 0x80, 0x52, 0xc0, 0x03, 0x5f, 0xd6];

pub fn write_return_42(writable: &mut WritableCode) -> Result<(), CodeMemoryError> {
    writable.write(0, RETURN_42)
}

#[cfg(all(target_endian = "little", target_arch = "x86_64"))]
pub fn write_return(
    writable: &mut WritableCode,
    offset: usize,
    value: i32,
) -> Result<(), CodeMemoryError> {
    let mut code = [0_u8; 6];
    code[0] = 0xb8;
    code[1..5].copy_from_slice(&value.to_le_bytes());
    code[5] = 0xc3;
    writable.write(offset, &code)
}

#[cfg(all(target_endian = "little", target_arch = "aarch64"))]
pub fn write_return(
    writable: &mut WritableCode,
    offset: usize,
    value: i32,
) -> Result<(), CodeMemoryError> {
    assert!((0..=u16::MAX as i32).contains(&value));
    let instruction = 0x5280_0000_u32 | ((value as u32) << 5);
    let mut code = [0_u8; 8];
    code[..4].copy_from_slice(&instruction.to_le_bytes());
    code[4..].copy_from_slice(&0xd65f_03c0_u32.to_le_bytes());
    writable.write(offset, &code)
}
