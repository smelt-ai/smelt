//! 调 APNs：用 `.p8` 私钥签提供方令牌（ES256 JWT），经 HTTP/2 发推送。
//!
//! 文档：<https://developer.apple.com/documentation/usernotifications/sending-notification-requests-to-apns>

use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair};
use smelt_push::{ApnsEnvironment, SendRequest};

use crate::handle::Device;

/// Apple 要求提供方令牌 20 到 60 分钟之间换一次：太勤会被限流，过期会被拒。
const TOKEN_LIFETIME: Duration = Duration::from_secs(40 * 60);
/// 手机离线时 APNs 最多替我们留这么久；再晚送到的审批提醒已经没用了。
const EXPIRATION: Duration = Duration::from_secs(60 * 60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Delivered,
    /// 令牌失效，或不属于这个环境 / topic。
    Unregistered,
    BadRequest(String),
    /// 网络、限流、APNs 故障，或提供方令牌刚好过期：可以重试。
    Retry(String),
}

pub struct Apns {
    client: reqwest::Client,
    key: EcdsaKeyPair,
    rng: SystemRandom,
    key_id: String,
    team_id: String,
    token: Mutex<Option<(String, Instant)>>,
}

impl Apns {
    /// `p8` 是 Apple 后台下载的 `AuthKey_XXXX.p8` 的内容（PKCS#8 PEM）。
    pub fn new(p8: &str, key_id: String, team_id: String) -> Result<Self, String> {
        let der = pem_body(p8)?;
        let rng = SystemRandom::new();
        let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &der, &rng)
            .map_err(|error| format!("APNs 私钥读不了：{error}"))?;
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|error| format!("建不了 HTTP 客户端：{error}"))?;
        Ok(Self {
            client,
            key,
            rng,
            key_id,
            team_id,
            token: Mutex::new(None),
        })
    }

    fn provider_token(&self) -> Result<String, String> {
        let mut cached = self.token.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((token, issued)) = cached.as_ref()
            && issued.elapsed() < TOKEN_LIFETIME
        {
            return Ok(token.clone());
        }
        let token = self.sign(unix_now())?;
        *cached = Some((token.clone(), Instant::now()));
        Ok(token)
    }

    fn sign(&self, issued_at: u64) -> Result<String, String> {
        let header = serde_json::json!({ "alg": "ES256", "kid": self.key_id });
        let claims = serde_json::json!({ "iss": self.team_id, "iat": issued_at });
        let signing_input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let signature = self
            .key
            .sign(&self.rng, signing_input.as_bytes())
            .map_err(|_| "APNs 提供方令牌签名失败".to_string())?;
        Ok(format!(
            "{signing_input}.{}",
            URL_SAFE_NO_PAD.encode(signature.as_ref())
        ))
    }

    fn forget_provider_token(&self) {
        *self.token.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    pub async fn send(&self, device: &Device, request: &SendRequest) -> Outcome {
        let token = match self.provider_token() {
            Ok(token) => token,
            Err(error) => return Outcome::Retry(error),
        };
        let host = match device.environment {
            ApnsEnvironment::Production => "api.push.apple.com",
            ApnsEnvironment::Sandbox => "api.sandbox.push.apple.com",
        };
        let expiration = unix_now() + EXPIRATION.as_secs();
        let response = self
            .client
            .post(format!("https://{host}/3/device/{}", device.token))
            .header("authorization", format!("bearer {token}"))
            .header("apns-topic", &device.topic)
            .header("apns-push-type", "alert")
            .header("apns-priority", "10")
            .header("apns-expiration", expiration.to_string())
            .header("apns-collapse-id", &request.collapse_id)
            .header("apns-id", &request.apns_id)
            .json(&smelt_push::apns_payload(request))
            .send()
            .await;
        let response = match response {
            Ok(response) => response,
            Err(error) => return Outcome::Retry(format!("连不上 APNs：{error}")),
        };
        let status = response.status().as_u16();
        if status == 200 {
            return Outcome::Delivered;
        }
        let reason = response
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|body| body.get("reason")?.as_str().map(str::to_string))
            .unwrap_or_default();
        let outcome = outcome_of(status, &reason);
        if reason == "ExpiredProviderToken" || reason == "InvalidProviderToken" {
            self.forget_provider_token();
        }
        outcome
    }
}

fn outcome_of(status: u16, reason: &str) -> Outcome {
    match (status, reason) {
        (410, _) | (400, "BadDeviceToken" | "DeviceTokenNotForTopic") => Outcome::Unregistered,
        (403, "ExpiredProviderToken" | "InvalidProviderToken") => {
            Outcome::Retry(format!("APNs 拒了提供方令牌：{reason}"))
        }
        (429, _) | (500..=599, _) => Outcome::Retry(format!("APNs {status} {reason}")),
        // 其余 403（证书、topic 没开推送）是中继自己配错了，换个时间重试也没用，但不是调用方的错：
        // 让运行时退避重试，运维看日志修。
        (403, _) => Outcome::Retry(format!("APNs 拒绝：{reason}")),
        _ => Outcome::BadRequest(format!("APNs {status} {reason}")),
    }
}

fn pem_body(pem: &str) -> Result<Vec<u8>, String> {
    let body: String = pem
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("-----"))
        .collect();
    STANDARD
        .decode(body)
        .map_err(|error| format!("APNs 私钥不是 PEM：{error}"))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
pub(crate) fn test_p8() -> String {
    let rng = SystemRandom::new();
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
    format!(
        "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
        STANDARD.encode(pkcs8.as_ref())
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{ECDSA_P256_SHA256_FIXED, KeyPair as _, UnparsedPublicKey};

    #[test]
    fn provider_tokens_are_es256_jwts_signed_by_the_key() {
        let apns = Apns::new(&test_p8(), "KEY123".into(), "TEAM456".into()).unwrap();
        let token = apns.sign(1_700_000_000).unwrap();
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3);
        let header: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
        assert_eq!(
            header,
            serde_json::json!({ "alg": "ES256", "kid": "KEY123" })
        );
        let claims: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(
            claims,
            serde_json::json!({ "iss": "TEAM456", "iat": 1_700_000_000 })
        );
        let signature = URL_SAFE_NO_PAD.decode(parts[2]).unwrap();
        assert_eq!(signature.len(), 64, "JWS 用定长 r‖s");
        UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, apns.key.public_key().as_ref())
            .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
            .unwrap();
    }

    #[test]
    fn provider_tokens_are_cached_until_forgotten() {
        let apns = Apns::new(&test_p8(), "K".into(), "T".into()).unwrap();
        let first = apns.provider_token().unwrap();
        assert_eq!(apns.provider_token().unwrap(), first);
        apns.forget_provider_token();
        assert!(apns.token.lock().unwrap().is_none());
    }

    #[test]
    fn apns_statuses_map_to_outcomes() {
        assert_eq!(outcome_of(410, "Unregistered"), Outcome::Unregistered);
        assert_eq!(outcome_of(400, "BadDeviceToken"), Outcome::Unregistered);
        assert_eq!(
            outcome_of(400, "DeviceTokenNotForTopic"),
            Outcome::Unregistered
        );
        assert!(matches!(
            outcome_of(400, "PayloadTooLarge"),
            Outcome::BadRequest(_)
        ));
        assert!(matches!(
            outcome_of(403, "ExpiredProviderToken"),
            Outcome::Retry(_)
        ));
        assert!(matches!(
            outcome_of(429, "TooManyRequests"),
            Outcome::Retry(_)
        ));
        assert!(matches!(outcome_of(503, ""), Outcome::Retry(_)));
    }

    #[test]
    fn rejects_keys_that_are_not_pem_pkcs8() {
        assert!(Apns::new("not a key", "K".into(), "T".into()).is_err());
        assert!(
            Apns::new(
                "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----",
                "K".into(),
                "T".into()
            )
            .is_err()
        );
    }
}
