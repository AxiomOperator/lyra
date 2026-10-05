//! Web Push (RFC 8030) with VAPID (RFC 8292) and payload encryption
//! (RFC 8291, aes128gcm): notifications reach a phone through its browser's
//! push service (Google's, Apple's, Mozilla's), which only sees ciphertext.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes128Gcm, Nonce};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use hkdf::Hkdf;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::{PublicKey, ecdh::EphemeralSecret};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

/// A browser's push subscription, as `PushSubscription.toJSON()` gives it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Subscription {
    pub endpoint: String,
    pub keys: Keys,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Keys {
    pub p256dh: String,
    pub auth: String,
}

/// The server's VAPID identity: a P-256 key the push services know it by.
pub struct Vapid {
    key: SigningKey,
}

impl Vapid {
    pub fn generate() -> Self {
        Self { key: SigningKey::random(&mut OsRng) }
    }

    pub fn from_base64(private: &str) -> Result<Self, String> {
        let bytes = B64.decode(private.trim()).map_err(|e| e.to_string())?;
        Ok(Self { key: SigningKey::from_slice(&bytes).map_err(|e| e.to_string())? })
    }

    pub fn private_base64(&self) -> String {
        B64.encode(self.key.to_bytes())
    }

    /// The public key browsers subscribe with (`applicationServerKey`).
    pub fn public_base64(&self) -> String {
        B64.encode(self.key.verifying_key().to_encoded_point(false).as_bytes())
    }

    /// The `Authorization` header for a push service: a signed, short-lived
    /// JWT for the endpoint's origin.
    pub fn authorization(&self, endpoint: &str, subject: &str) -> Result<String, String> {
        let origin = origin(endpoint)?;
        let exp = chrono::Utc::now().timestamp() + 12 * 3600;
        let header = B64.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
        let claims = B64.encode(serde_json::json!({ "aud": origin, "exp": exp, "sub": subject }).to_string());
        let signing_input = format!("{header}.{claims}");
        let signature: Signature = self.key.sign(signing_input.as_bytes());
        Ok(format!("vapid t={signing_input}.{}, k={}", B64.encode(signature.to_bytes()), self.public_base64()))
    }
}

/// `https://fcm.googleapis.com/fcm/send/…` → `https://fcm.googleapis.com`.
fn origin(endpoint: &str) -> Result<String, String> {
    let (scheme, rest) = endpoint.split_once("://").ok_or("the push endpoint isn't a URL")?;
    if scheme != "https" {
        return Err("push endpoints must be https".into());
    }
    let host = rest.split('/').next().unwrap_or("");
    Ok(format!("{scheme}://{host}"))
}

fn hkdf(salt: &[u8], ikm: &[u8], info: &[u8], len: usize) -> Vec<u8> {
    let mut out = vec![0; len];
    Hkdf::<Sha256>::new(Some(salt), ikm).expand(info, &mut out).expect("valid length");
    out
}

/// Encrypt a payload for one subscription (RFC 8291 / RFC 8188 aes128gcm, a
/// single record). Returns the request body.
pub fn encrypt(sub: &Subscription, payload: &[u8]) -> Result<Vec<u8>, String> {
    let ua_public_bytes = B64.decode(sub.keys.p256dh.trim_end_matches('=')).map_err(|e| format!("bad p256dh: {e}"))?;
    let auth = B64.decode(sub.keys.auth.trim_end_matches('=')).map_err(|e| format!("bad auth: {e}"))?;
    let ua_public = PublicKey::from_sec1_bytes(&ua_public_bytes).map_err(|e| format!("bad p256dh: {e}"))?;
    let secret = EphemeralSecret::random(&mut OsRng);
    let as_public = secret.public_key().to_encoded_point(false);
    let shared = secret.diffie_hellman(&ua_public);
    let mut key_info = b"WebPush: info\0".to_vec();
    key_info.extend_from_slice(&ua_public_bytes);
    key_info.extend_from_slice(as_public.as_bytes());
    let ikm = hkdf(&auth, shared.raw_secret_bytes(), &key_info, 32);
    let mut salt = [0u8; 16];
    OsRng.fill_bytes(&mut salt);
    let cek = hkdf(&salt, &ikm, b"Content-Encoding: aes128gcm\0", 16);
    let nonce = hkdf(&salt, &ikm, b"Content-Encoding: nonce\0", 12);
    let mut plain = payload.to_vec();
    plain.push(2); // the last (only) record
    let cipher = Aes128Gcm::new_from_slice(&cek).map_err(|e| e.to_string())?;
    let sealed = cipher.encrypt(Nonce::from_slice(&nonce), plain.as_slice()).map_err(|e| e.to_string())?;
    let mut body = salt.to_vec();
    body.extend_from_slice(&4096u32.to_be_bytes());
    body.push(as_public.as_bytes().len() as u8);
    body.extend_from_slice(as_public.as_bytes());
    body.extend_from_slice(&sealed);
    Ok(body)
}

/// What sending one notification came to.
#[derive(Debug, PartialEq)]
pub enum Sent {
    Ok,
    /// The subscription is gone (unsubscribed, app removed): forget it.
    Gone,
    Failed(String),
}

/// Send a notification (blocking; call from a worker thread).
pub fn send(vapid: &Vapid, subject: &str, sub: &Subscription, payload: &[u8]) -> Sent {
    let body = match encrypt(sub, payload) {
        Ok(b) => b,
        Err(e) => return Sent::Failed(e),
    };
    let auth = match vapid.authorization(&sub.endpoint, subject) {
        Ok(a) => a,
        Err(e) => return Sent::Failed(e),
    };
    let client = match reqwest::blocking::Client::builder().timeout(std::time::Duration::from_secs(20)).build() {
        Ok(c) => c,
        Err(e) => return Sent::Failed(e.to_string()),
    };
    let resp = client
        .post(&sub.endpoint)
        .header("Authorization", auth)
        .header("Content-Encoding", "aes128gcm")
        .header("Content-Type", "application/octet-stream")
        .header("TTL", "86400")
        .header("Urgency", "high")
        .body(body)
        .send();
    match resp {
        Ok(r) if r.status().is_success() => Sent::Ok,
        Ok(r) if matches!(r.status().as_u16(), 404 | 410) => Sent::Gone,
        Ok(r) => {
            let status = r.status();
            Sent::Failed(format!("{status}: {}", r.text().unwrap_or_default().chars().take(200).collect::<String>()))
        }
        Err(e) => Sent::Failed(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::SecretKey;
    use p256::ecdsa::VerifyingKey;
    use p256::ecdsa::signature::Verifier;

    /// Decrypt as the browser would (RFC 8291 from the receiving side).
    fn decrypt(ua_secret: &SecretKey, auth: &[u8], body: &[u8]) -> Vec<u8> {
        let salt = &body[..16];
        let rs = u32::from_be_bytes(body[16..20].try_into().unwrap());
        assert_eq!(rs, 4096);
        let idlen = body[20] as usize;
        let as_public_bytes = &body[21..21 + idlen];
        let sealed = &body[21 + idlen..];
        let as_public = PublicKey::from_sec1_bytes(as_public_bytes).unwrap();
        let shared = p256::ecdh::diffie_hellman(ua_secret.to_nonzero_scalar(), as_public.as_affine());
        let ua_public = ua_secret.public_key().to_encoded_point(false);
        let mut key_info = b"WebPush: info\0".to_vec();
        key_info.extend_from_slice(ua_public.as_bytes());
        key_info.extend_from_slice(as_public_bytes);
        let ikm = hkdf(auth, shared.raw_secret_bytes(), &key_info, 32);
        let cek = hkdf(salt, &ikm, b"Content-Encoding: aes128gcm\0", 16);
        let nonce = hkdf(salt, &ikm, b"Content-Encoding: nonce\0", 12);
        let mut plain = Aes128Gcm::new_from_slice(&cek).unwrap().decrypt(Nonce::from_slice(&nonce), sealed).unwrap();
        assert_eq!(plain.pop(), Some(2), "last-record delimiter");
        plain
    }

    #[test]
    fn a_browser_can_decrypt_what_we_send() {
        let ua = SecretKey::random(&mut OsRng);
        let mut auth = [0u8; 16];
        OsRng.fill_bytes(&mut auth);
        let sub = Subscription {
            endpoint: "https://push.example.com/send/abc".into(),
            keys: Keys { p256dh: B64.encode(ua.public_key().to_encoded_point(false).as_bytes()), auth: B64.encode(auth) },
        };
        let payload = br#"{"title":"lyra","body":"Operator wants to restart nginx"}"#;
        let body = encrypt(&sub, payload).unwrap();
        assert_eq!(decrypt(&ua, &auth, &body), payload);
        // Padded base64 from some browsers is accepted too.
        let padded = Subscription { keys: Keys { auth: format!("{}==", sub.keys.auth), ..sub.keys.clone() }, ..sub.clone() };
        assert!(encrypt(&padded, b"x").is_ok());
    }

    /// Cross-check with an independent implementation: `LYRA_PUSH_FIXTURE=dir`
    /// holding `sub.json` (from a Node script with its own keys) gets
    /// `body.bin`, which the script then decrypts with WebCrypto.
    #[test]
    #[ignore]
    fn fixture_for_an_independent_decrypt() {
        let dir = std::path::PathBuf::from(std::env::var("LYRA_PUSH_FIXTURE").expect("LYRA_PUSH_FIXTURE"));
        let sub: Subscription = serde_json::from_str(&std::fs::read_to_string(dir.join("sub.json")).unwrap()).unwrap();
        std::fs::write(dir.join("body.bin"), encrypt(&sub, "hello from lyra ✓".as_bytes()).unwrap()).unwrap();
    }

    #[test]
    fn vapid_tokens_are_signed_for_the_endpoints_origin() {
        let v = Vapid::generate();
        let again = Vapid::from_base64(&v.private_base64()).unwrap();
        assert_eq!(v.public_base64(), again.public_base64());
        let header = v.authorization("https://fcm.googleapis.com/fcm/send/xyz", "https://lyra.example.com").unwrap();
        let token = header.strip_prefix("vapid t=").unwrap().split(", k=").next().unwrap();
        let parts: Vec<&str> = token.split('.').collect();
        let claims: serde_json::Value = serde_json::from_slice(&B64.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(claims["aud"], "https://fcm.googleapis.com");
        assert_eq!(claims["sub"], "https://lyra.example.com");
        let public = B64.decode(v.public_base64()).unwrap();
        let key = VerifyingKey::from_sec1_bytes(&public).unwrap();
        let sig = Signature::from_slice(&B64.decode(parts[2]).unwrap()).unwrap();
        assert!(key.verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig).is_ok());
        assert!(v.authorization("http://insecure/x", "s").is_err());
    }
}
