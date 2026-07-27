//! E2EE 加密（与 Orca/tweetnacl 兼容）
//!
//! 使用 X25519 ECDH 密钥交换 + XChaCha20-Poly1305 认证加密。
//! 与 JavaScript 端的 tweetnacl 互操作。

use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    XChaCha20Poly1305, XNonce,
};
use rand::RngCore;
use x25519_dalek::{PublicKey, StaticSecret};

use crate::api::{HostConfig, KeyPair};

/// 生成 X25519 密钥对
pub fn generate_keypair() -> KeyPair {
    let mut rng = rand::thread_rng();
    let mut secret_bytes = [0u8; 32];
    rng.fill_bytes(&mut secret_bytes);
    let secret = StaticSecret::from(secret_bytes);
    let public = PublicKey::from(&secret);

    KeyPair {
        public_key_b64: BASE64.encode(public.as_bytes()),
        secret_key_b64: BASE64.encode(secret_bytes),
    }
}

/// 从 Base64 解码公钥
pub fn public_key_from_b64(b64: &str) -> Result<PublicKey> {
    let bytes = BASE64.decode(b64)?;
    if bytes.len() != 32 {
        return Err(anyhow!("Invalid public key length: {}", bytes.len()));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(PublicKey::from(arr))
}

/// 从 Base64 解码私钥
pub fn secret_key_from_b64(b64: &str) -> Result<StaticSecret> {
    let bytes = BASE64.decode(b64)?;
    if bytes.len() != 32 {
        return Err(anyhow!("Invalid secret key length: {}", bytes.len()));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Ok(StaticSecret::from(arr))
}

/// 派生共享密钥（ECDH）
pub fn derive_shared_key(our_secret: &StaticSecret, peer_public: &PublicKey) -> [u8; 32] {
    let shared = our_secret.diffie_hellman(peer_public);
    *shared.as_bytes()
}

/// 加密消息
///
/// 格式：`base64(nonce[24] + ciphertext + tag[16])`
pub fn encrypt(plaintext: &str, shared_key: &[u8; 32]) -> Result<String> {
    let cipher = XChaCha20Poly1305::new(shared_key.into());

    let mut nonce_bytes = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = XNonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .map_err(|e| anyhow!("Encryption failed: {}", e))?;

    let mut bundle = Vec::with_capacity(24 + ciphertext.len());
    bundle.extend_from_slice(&nonce_bytes);
    bundle.extend_from_slice(&ciphertext);

    Ok(BASE64.encode(&bundle))
}

/// 解密消息
pub fn decrypt(encrypted: &str, shared_key: &[u8; 32]) -> Result<String> {
    let bundle = BASE64.decode(encrypted)?;
    if bundle.len() < 24 + 16 {
        return Err(anyhow!("Ciphertext too short"));
    }

    let nonce = XNonce::from_slice(&bundle[..24]);
    let ciphertext = &bundle[24..];

    let cipher = XChaCha20Poly1305::new(shared_key.into());
    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| anyhow!("Decryption failed: {}", e))?;

    Ok(String::from_utf8(plaintext)?)
}

/// 加密字节
pub fn encrypt_bytes(plaintext: &[u8], shared_key: &[u8; 32]) -> Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new(shared_key.into());

    let mut nonce_bytes = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = XNonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| anyhow!("Encryption failed: {}", e))?;

    let mut bundle = Vec::with_capacity(24 + ciphertext.len());
    bundle.extend_from_slice(&nonce_bytes);
    bundle.extend_from_slice(&ciphertext);

    Ok(bundle)
}

/// 解密字节
pub fn decrypt_bytes(bundle: &[u8], shared_key: &[u8; 32]) -> Result<Vec<u8>> {
    if bundle.len() < 24 + 16 {
        return Err(anyhow!("Ciphertext too short"));
    }

    let nonce = XNonce::from_slice(&bundle[..24]);
    let ciphertext = &bundle[24..];

    let cipher = XChaCha20Poly1305::new(shared_key.into());
    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| anyhow!("Decryption failed: {}", e))?;

    Ok(plaintext)
}

/// 解析配对二维码
///
/// 二维码格式（JSON）：
/// ```json
/// {
///   "endpoint": "ws://192.168.1.100:6768",
///   "token": "...",
///   "publicKey": "base64...",
///   "name": "My Mac"
/// }
/// ```
pub fn parse_pairing_qr(qr_data: &str) -> Result<HostConfig> {
    #[derive(serde::Deserialize)]
    struct QrPayload {
        endpoint: String,
        token: String,
        #[serde(rename = "publicKey")]
        public_key: String,
        name: Option<String>,
    }

    let payload: QrPayload = serde_json::from_str(qr_data)
        .map_err(|e| anyhow!("Invalid QR code format: {}", e))?;

    // 验证公钥格式
    let _ = public_key_from_b64(&payload.public_key)?;

    // 生成唯一 ID
    let id = format!(
        "{:x}",
        md5::compute(format!("{}:{}", payload.endpoint, payload.token))
    );

    Ok(HostConfig {
        id,
        name: payload.name.unwrap_or_else(|| "Desktop".to_string()),
        endpoint: payload.endpoint,
        public_key_b64: payload.public_key,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keypair_generation() {
        let kp = generate_keypair();
        assert!(!kp.public_key_b64.is_empty());
        assert!(!kp.secret_key_b64.is_empty());

        // 验证可以解码回来
        let pk = public_key_from_b64(&kp.public_key_b64).unwrap();
        let sk = secret_key_from_b64(&kp.secret_key_b64).unwrap();

        // 验证公私钥匹配
        let derived_pk = PublicKey::from(&sk);
        assert_eq!(pk.as_bytes(), derived_pk.as_bytes());
    }

    #[test]
    fn test_encrypt_decrypt() {
        let kp1 = generate_keypair();
        let kp2 = generate_keypair();

        let sk1 = secret_key_from_b64(&kp1.secret_key_b64).unwrap();
        let pk2 = public_key_from_b64(&kp2.public_key_b64).unwrap();
        let sk2 = secret_key_from_b64(&kp2.secret_key_b64).unwrap();
        let pk1 = public_key_from_b64(&kp1.public_key_b64).unwrap();

        let shared1 = derive_shared_key(&sk1, &pk2);
        let shared2 = derive_shared_key(&sk2, &pk1);

        // 共享密钥应该相同
        assert_eq!(shared1, shared2);

        // 加解密测试
        let plaintext = "Hello, Smelt!";
        let encrypted = encrypt(plaintext, &shared1).unwrap();
        let decrypted = decrypt(&encrypted, &shared2).unwrap();

        assert_eq!(plaintext, decrypted);
    }
}
