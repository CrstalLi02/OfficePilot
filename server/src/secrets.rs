//! 加密存储用户填写的密钥（IM 机器人的 App Secret、MCP 服务的请求头 / 环境变量等）。
//!
//! AES-256-GCM，密钥由服务端 JWT 密钥派生；格式 `v1:base64(nonce || 密文)`。
//! 更换 JWT 密钥后旧数据无法解密，调用方需要提示用户重新填写。

use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use sha2::{Digest, Sha256};

/// 由服务端密钥派生的加密器；不同用途共用同一个派生前缀，保持与已存数据兼容
fn cipher_for(server_secret: &str) -> Aes256Gcm {
    let key = Sha256::digest(format!("walioffice-im-secrets:{server_secret}").as_bytes());
    Aes256Gcm::new_from_slice(&key).expect("32 字节密钥")
}

fn seal_with(cipher: &Aes256Gcm, plain: &str) -> String {
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let mut out = nonce.to_vec();
    out.extend(cipher.encrypt(&nonce, plain.as_bytes()).expect("加密不会失败"));
    format!("v1:{}", base64::engine::general_purpose::STANDARD.encode(out))
}

fn open_with(cipher: &Aes256Gcm, sealed: &str) -> Option<String> {
    let raw = base64::engine::general_purpose::STANDARD.decode(sealed.strip_prefix("v1:")?).ok()?;
    if raw.len() < 12 {
        return None;
    }
    let (nonce, data) = raw.split_at(12);
    let plain = cipher.decrypt(Nonce::from_slice(nonce), data).ok()?;
    String::from_utf8(plain).ok()
}

/// 加密
pub fn seal(plain: &str) -> String {
    seal_with(&cipher_for(&crate::config::config().jwt_secret), plain)
}

/// 解密；失败（例如更换了 JWT 密钥）时返回 None
pub fn open(sealed: &str) -> Option<String> {
    open_with(&cipher_for(&crate::config::config().jwt_secret), sealed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_need_same_key() {
        let cipher = cipher_for("server-secret");
        let sealed = seal_with(&cipher, r#"{"app_secret":"abc"}"#);
        assert!(sealed.starts_with("v1:") && !sealed.contains("abc"));
        assert_ne!(sealed, seal_with(&cipher, r#"{"app_secret":"abc"}"#), "每次用新的 nonce");
        assert_eq!(open_with(&cipher, &sealed).as_deref(), Some(r#"{"app_secret":"abc"}"#));
        assert_eq!(open_with(&cipher_for("rotated"), &sealed), None);
        assert_eq!(open_with(&cipher, "garbage"), None);
    }
}
