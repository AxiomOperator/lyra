//! Signing in with Microsoft (Entra ID): OpenID Connect's authorization code
//! flow with PKCE. The browser goes to Microsoft and comes back to
//! `/auth/callback` with a code; lyra trades the code (with its client
//! secret) for an id_token straight from Microsoft's token endpoint over TLS,
//! so the token's origin is known without checking its signature (OpenID
//! Connect Core 3.1.3.7, step 6); its issuer, audience, tenant, expiry and
//! nonce are checked here.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// `[web.entra]`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Entra {
    /// The directory (tenant) id: a GUID, so only that organization's accounts sign in.
    pub tenant: String,
    /// The app registration's (client) id.
    pub client_id: String,
    /// The email whose first sign-in becomes the owner (the first admin).
    pub owner_email: String,
    /// The client secret: from lyra's secrets file, never the config.
    #[serde(skip)]
    pub secret: Option<String>,
}

impl Entra {
    pub fn ready(&self) -> bool {
        !self.tenant.trim().is_empty() && !self.client_id.trim().is_empty() && self.secret.is_some()
    }
}

/// A sign-in on its way: what to check when Microsoft sends the browser back.
#[derive(Debug, Clone)]
pub struct Pending {
    pub verifier: String,
    pub nonce: String,
    /// What to call the device it signs in.
    pub device: String,
    pub created: std::time::Instant,
    /// Not a sign-in: this user connecting their Microsoft account to a
    /// service (their calendar), with the account's object id it must be.
    pub connect: Option<(String, String)>,
}

/// Signing in only.
pub const SIGN_IN: &str = "openid profile email";
/// Their calendar only (connections made before mail).
pub const CALENDAR: &str = "openid profile email offline_access Calendars.ReadWrite";
/// Their Outlook: calendar and mail, kept up with a refresh token.
pub const OUTLOOK: &str = "openid profile email offline_access Calendars.ReadWrite Mail.ReadWrite Mail.Send";

/// PKCE: the code challenge for a verifier.
pub fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

pub fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

/// Where to send the browser.
pub fn authorize_url(e: &Entra, redirect: &str, state: &str, nonce: &str, verifier: &str) -> String {
    authorize_url_for(e, redirect, state, nonce, verifier, SIGN_IN)
}

/// The same, asking for `scope` (`CALENDAR` to connect a calendar).
pub fn authorize_url_for(e: &Entra, redirect: &str, state: &str, nonce: &str, verifier: &str, scope: &str) -> String {
    format!(
        "https://login.microsoftonline.com/{}/oauth2/v2.0/authorize?client_id={}&response_type=code&redirect_uri={}&response_mode=query&scope={}&state={}&nonce={}&code_challenge={}&code_challenge_method=S256&prompt=select_account",
        encode(e.tenant.trim()),
        encode(e.client_id.trim()),
        encode(redirect),
        encode(scope),
        encode(state),
        encode(nonce),
        challenge(verifier)
    )
}

pub fn token_url(e: &Entra) -> String {
    format!("https://login.microsoftonline.com/{}/oauth2/v2.0/token", encode(e.tenant.trim()))
}

/// The form posted to the token endpoint.
pub fn token_form(e: &Entra, code: &str, redirect: &str, verifier: &str) -> String {
    token_form_for(e, code, redirect, verifier, SIGN_IN)
}

pub fn token_form_for(e: &Entra, code: &str, redirect: &str, verifier: &str, scope: &str) -> String {
    [
        ("grant_type", "authorization_code"),
        ("client_id", e.client_id.trim()),
        ("client_secret", e.secret.as_deref().unwrap_or("")),
        ("code", code),
        ("redirect_uri", redirect),
        ("code_verifier", verifier),
        ("scope", scope),
    ]
    .iter()
    .map(|(k, v)| format!("{k}={}", encode(v)))
    .collect::<Vec<_>>()
    .join("&")
}

/// Who signed in, from a checked id_token.
#[derive(Debug, Clone, PartialEq)]
pub struct Person {
    /// Their object id: the same in every app of the tenant.
    pub oid: String,
    pub tenant: String,
    pub name: String,
    pub email: String,
    /// A guest from another organization (B2B): never the owner.
    pub guest: bool,
}

/// The claims of an id_token (its middle part), unverified.
pub fn claims(id_token: &str) -> Result<Value, String> {
    let payload = id_token.split('.').nth(1).ok_or("not an id_token")?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).map_err(|e| format!("id_token: {e}"))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("id_token: {e}"))
}

/// Check an id_token that came straight from the token endpoint: it's for
/// this app, from this tenant, current, and answers this sign-in.
pub fn check(e: &Entra, id_token: &str, nonce: &str, now: i64) -> Result<Person, String> {
    let c = claims(id_token)?;
    let s = |k: &str| c[k].as_str().unwrap_or("").to_string();
    let tenant = e.tenant.trim().to_lowercase();
    if s("tid").to_lowercase() != tenant {
        return Err("that account isn't in this organization".into());
    }
    if s("iss").to_lowercase() != format!("https://login.microsoftonline.com/{tenant}/v2.0") {
        return Err("the sign-in came from somewhere unexpected".into());
    }
    let aud_ok = match &c["aud"] {
        Value::String(a) => a == e.client_id.trim(),
        Value::Array(list) => list.iter().any(|a| a.as_str() == Some(e.client_id.trim())),
        _ => false,
    };
    if !aud_ok {
        return Err("the sign-in was for another app".into());
    }
    let exp = c["exp"].as_i64().unwrap_or(0);
    if exp + 60 < now || c["nbf"].as_i64().is_some_and(|nbf| nbf - 60 > now) {
        return Err("the sign-in expired: try again".into());
    }
    if s("nonce") != nonce {
        return Err("the sign-in didn't match: try again".into());
    }
    let oid = s("oid");
    if oid.is_empty() {
        return Err("Microsoft didn't say who you are".into());
    }
    let email = [s("email"), s("preferred_username"), s("upn")].into_iter().find(|x| x.contains('@')).unwrap_or_default();
    let name = Some(s("name")).filter(|n| !n.is_empty()).unwrap_or_else(|| email.clone());
    let guest = c["acct"].as_i64() == Some(1) || [s("upn"), s("preferred_username"), s("unique_name")].iter().any(|u| u.contains("#EXT#"));
    Ok(Person { oid, tenant: s("tid"), name, email, guest })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TENANT: &str = "11111111-2222-3333-4444-555555555555";

    fn entra() -> Entra {
        Entra { tenant: TENANT.into(), client_id: "app-1".into(), owner_email: "me@fbcad.org".into(), secret: Some("s3cret".into()) }
    }

    fn token(claims: Value) -> String {
        format!("{}.{}.sig", URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256"}"#), URL_SAFE_NO_PAD.encode(claims.to_string()))
    }

    fn good() -> Value {
        serde_json::json!({
            "iss": format!("https://login.microsoftonline.com/{TENANT}/v2.0"), "aud": "app-1", "tid": TENANT,
            "exp": 2_000, "nbf": 900, "nonce": "n1", "oid": "oid-1", "name": "Dana Doe", "preferred_username": "dana@fbcad.org",
        })
    }

    #[test]
    fn a_good_sign_in_says_who() {
        let p = check(&entra(), &token(good()), "n1", 1_000).unwrap();
        assert_eq!(p, Person { oid: "oid-1".into(), tenant: TENANT.into(), name: "Dana Doe".into(), email: "dana@fbcad.org".into(), guest: false });
        let mut g = good();
        g["upn"] = "boss_other.com#EXT#@fbcad.onmicrosoft.com".into();
        assert!(check(&entra(), &token(g), "n1", 1_000).unwrap().guest, "a guest is known as one");
    }

    #[test]
    fn anything_off_is_refused() {
        let with = |k: &str, v: Value| {
            let mut c = good();
            c[k] = v;
            check(&entra(), &token(c), "n1", 1_000)
        };
        assert!(with("tid", "other".into()).unwrap_err().contains("organization"));
        assert!(with("iss", "https://evil.example/v2.0".into()).is_err());
        assert!(with("aud", "another-app".into()).unwrap_err().contains("another app"));
        assert!(with("exp", 100.into()).unwrap_err().contains("expired"));
        assert!(with("nonce", "replayed".into()).is_err());
        assert!(with("oid", "".into()).is_err());
        assert!(check(&entra(), "garbage", "n1", 1_000).is_err());
    }

    #[test]
    fn the_way_there_carries_pkce_and_state() {
        // RFC 7636 appendix B.
        assert_eq!(challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        let url = authorize_url(&entra(), "https://lyra.fbcad.org/auth/callback", "st", "n1", "v");
        assert!(url.starts_with(&format!("https://login.microsoftonline.com/{TENANT}/oauth2/v2.0/authorize?client_id=app-1&")));
        assert!(url.contains("redirect_uri=https%3A%2F%2Flyra.fbcad.org%2Fauth%2Fcallback") && url.contains("state=st") && url.contains("code_challenge_method=S256"));
        assert!(token_form(&entra(), "c&d", "r", "v").contains("code=c%26d"), "form values are escaped");
    }
}
