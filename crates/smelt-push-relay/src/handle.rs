//! 推送句柄：中继用自己的密钥把 APNs 登记加密成一串，交给手机分发给各台运行时。
//!
//! 中继因此不用存设备表；运行时库里只有句柄，没有裸令牌；拿着裸令牌的人也没法借这个中继
//! 给别人的手机发推送。换掉句柄密钥会让所有句柄作废（发送时回 410，手机下次启动重新登记）。

use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use smelt_push::ApnsEnvironment;

const NONCE_LEN: usize = 12;

/// 句柄里装的东西。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub token: String,
    pub environment: ApnsEnvironment,
    pub topic: String,
}

pub struct HandleKey(Aes256Gcm);

impl HandleKey {
    /// 密钥文件里是 32 字节随机数的 base64（`openssl rand -base64 32`）。
    pub fn from_base64(text: &str) -> Result<Self, String> {
        let bytes = STANDARD
            .decode(text.trim())
            .map_err(|error| format!("句柄密钥不是 base64：{error}"))?;
        Aes256Gcm::new_from_slice(&bytes)
            .map(Self)
            .map_err(|_| format!("句柄密钥要 32 字节，现在是 {} 字节", bytes.len()))
    }

    pub fn seal(&self, device: &Device) -> String {
        let plaintext = serde_json::to_vec(device).expect("device serializes");
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let ciphertext = self
            .0
            .encrypt(&nonce, plaintext.as_slice())
            .expect("AES-GCM encryption of a small buffer cannot fail");
        let mut sealed = nonce.to_vec();
        sealed.extend_from_slice(&ciphertext);
        URL_SAFE_NO_PAD.encode(sealed)
    }

    /// 不是这把密钥封的、被改过、格式不对的都返回 `None`。
    pub fn open(&self, handle: &str) -> Option<Device> {
        let sealed = URL_SAFE_NO_PAD.decode(handle).ok()?;
        if sealed.len() <= NONCE_LEN {
            return None;
        }
        let (nonce, ciphertext) = sealed.split_at(NONCE_LEN);
        let plaintext = self.0.decrypt(Nonce::from_slice(nonce), ciphertext).ok()?;
        serde_json::from_slice(&plaintext).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> HandleKey {
        HandleKey::from_base64(&STANDARD.encode([byte; 32])).unwrap()
    }

    fn device() -> Device {
        Device {
            token: "ab".repeat(32),
            environment: ApnsEnvironment::Production,
            topic: "ai.smelt.mobile".into(),
        }
    }

    #[test]
    fn handles_round_trip_and_differ_each_time() {
        let key = key(1);
        let handle = key.seal(&device());
        assert_eq!(key.open(&handle), Some(device()));
        assert_ne!(handle, key.seal(&device()), "每次换 nonce");
        assert!(!handle.contains(&device().token), "句柄里看不出令牌");
    }

    #[test]
    fn foreign_or_tampered_handles_do_not_open() {
        let handle = key(1).seal(&device());
        assert_eq!(key(2).open(&handle), None);
        let mut tampered = handle.into_bytes();
        let last = tampered.len() - 1;
        tampered[last] = if tampered[last] == b'A' { b'B' } else { b'A' };
        assert_eq!(key(1).open(&String::from_utf8(tampered).unwrap()), None);
        assert_eq!(key(1).open("not a handle!"), None);
        assert_eq!(key(1).open(""), None);
    }

    #[test]
    fn keys_must_be_32_bytes() {
        assert!(HandleKey::from_base64(&STANDARD.encode([0; 16])).is_err());
        assert!(HandleKey::from_base64("%%%").is_err());
    }
}
