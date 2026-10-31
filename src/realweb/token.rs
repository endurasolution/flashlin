//! RealWeb access tokens: standard HS256 JSON Web Tokens, so applications can
//! mint them with any JWT library (`jsonwebtoken` for Node, PyJWT, ...) using
//! the shared `realweb.secret`.
//!
//! ```json
//! { "sub": "user-42", "exp": 1767225600,
//!   "subscribe": ["chat/*", "user/user-42"], "publish": ["chat/*"] }
//! ```

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Clock skew tolerated on `exp` and `nbf`.
pub const LEEWAY_SECS: u64 = 30;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Claims {
    /// User id: shown to other clients as `from` and in presence lists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub: Option<String>,
    /// Expiry (Unix seconds). Required: a token that never expires cannot be
    /// revoked, so one without `exp` is rejected.
    pub exp: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nbf: Option<u64>,
    /// Channel patterns this token may subscribe to (`"chat/*"`, `"*"`, exact names).
    #[serde(default)]
    pub subscribe: Vec<String>,
    /// Channel patterns this token may publish to.
    #[serde(default)]
    pub publish: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenError {
    Malformed,
    BadSignature,
    Expired,
    NotYetValid,
}

impl TokenError {
    pub fn code(self) -> &'static str {
        match self {
            TokenError::Malformed => "token_malformed",
            TokenError::BadSignature => "token_invalid",
            TokenError::Expired => "token_expired",
            TokenError::NotYetValid => "token_not_yet_valid",
        }
    }
}

#[derive(Deserialize)]
struct Header {
    alg: String,
}

/// Verifies signature, algorithm and validity window, in that order: claims
/// are never parsed from an unauthenticated payload.
pub fn verify(token: &str, secret: &[u8], now: u64) -> Result<Claims, TokenError> {
    let mut parts = token.split('.');
    let (Some(header), Some(payload), Some(signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(TokenError::Malformed);
    };
    let decode = |part: &str| B64.decode(part).map_err(|_| TokenError::Malformed);
    // Only HS256 is accepted, which rules out `alg: none` and any attempt to
    // pass the secret off as a public key for another algorithm.
    let header: Header =
        serde_json::from_slice(&decode(header)?).map_err(|_| TokenError::Malformed)?;
    if header.alg != "HS256" {
        return Err(TokenError::Malformed);
    }
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(&token.as_bytes()[..token.len() - signature.len() - 1]);
    mac.verify_slice(&decode(signature)?)
        .map_err(|_| TokenError::BadSignature)?;

    let claims: Claims =
        serde_json::from_slice(&decode(payload)?).map_err(|_| TokenError::Malformed)?;
    if claims.exp.saturating_add(LEEWAY_SECS) <= now {
        return Err(TokenError::Expired);
    }
    if claims.nbf.is_some_and(|nbf| nbf > now + LEEWAY_SECS) {
        return Err(TokenError::NotYetValid);
    }
    if claims
        .sub
        .as_ref()
        .is_some_and(|s| s.is_empty() || s.len() > 128)
        || claims
            .subscribe
            .iter()
            .chain(&claims.publish)
            .any(|p| !super::valid_pattern(p))
    {
        return Err(TokenError::Malformed);
    }
    Ok(claims)
}

/// Signs `claims` with `secret`; used by `flashlin realweb token` and tests.
pub fn mint(claims: &Claims, secret: &[u8]) -> String {
    let header = B64.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
    let payload = B64.encode(serde_json::to_vec(claims).expect("claims serialize"));
    let signing_input = format!("{header}.{payload}");
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(signing_input.as_bytes());
    let signature = B64.encode(mac.finalize().into_bytes());
    format!("{signing_input}.{signature}")
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
