//! Remote API authentication: HMAC-SHA256 session tokens.
//!
//! A 256-bit secret lives in a key file on the show computer (never in the
//! show file, which gets copied around). The operator issues tokens from the
//! settings; a remote client presents one when it opens the WebSocket:
//!
//! ```text
//! token = "v1." expiry "." label "." hex(HMAC-SHA256(secret, "v1." expiry "." label))
//! ```
//!
//! `expiry` is Unix seconds, `label` names the client (`stage-left-ipad`).
//! Verification recomputes the MAC and compares it in constant time, then
//! checks the expiry. Nothing is stored per token: rotating the secret
//! revokes every token at once.

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;

const VERSION: &str = "v1";
const MAX_LABEL: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    #[error("no token presented")]
    Missing,
    #[error("malformed token")]
    Malformed,
    #[error("token signature does not match")]
    BadSignature,
    #[error("token expired")]
    Expired,
}

/// A verified token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenInfo {
    pub label: String,
    /// Unix seconds.
    pub expires: u64,
}

/// The token-signing secret.
#[derive(Clone)]
pub struct RemoteAuth {
    secret: [u8; 32],
}

impl std::fmt::Debug for RemoteAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RemoteAuth(..)") // never print the secret
    }
}

impl RemoteAuth {
    pub fn from_secret(secret: [u8; 32]) -> Self {
        Self { secret }
    }

    /// A fresh random secret from the operating system.
    pub fn generate() -> std::io::Result<Self> {
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).map_err(std::io::Error::other)?;
        Ok(Self { secret })
    }

    /// Loads the secret from `path` (hex), creating it on first use.
    pub fn load_or_create(path: &Path) -> std::io::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let bytes = from_hex(text.trim()).filter(|b| b.len() == 32).ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{} is not a LiveStage remote key", path.display()))
                })?;
                let mut secret = [0u8; 32];
                secret.copy_from_slice(&bytes);
                Ok(Self { secret })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::rotate(path),
            Err(e) => Err(e),
        }
    }

    /// Writes a new secret to `path`, revoking every token issued so far.
    pub fn rotate(path: &Path) -> std::io::Result<Self> {
        let auth = Self::generate()?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        write_private(&tmp, to_hex(&auth.secret).as_bytes())?;
        std::fs::rename(&tmp, path)?;
        Ok(auth)
    }

    /// Issues a token for `label`, valid for `valid_for` from `now`.
    pub fn issue(&self, label: &str, valid_for: Duration, now: SystemTime) -> String {
        let label: String =
            label.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')).take(MAX_LABEL).collect();
        let label = if label.is_empty() { "remote".to_string() } else { label };
        let expires = unix(now).saturating_add(valid_for.as_secs());
        let payload = format!("{VERSION}.{expires}.{label}");
        let mac = to_hex(&self.mac(&payload).finalize().into_bytes());
        format!("{payload}.{mac}")
    }

    /// Checks a token's signature (constant time) and expiry.
    pub fn verify(&self, token: &str, now: SystemTime) -> Result<TokenInfo, AuthError> {
        let token = token.trim();
        if token.is_empty() {
            return Err(AuthError::Missing);
        }
        let (payload, mac_hex) = token.rsplit_once('.').ok_or(AuthError::Malformed)?;
        let mac = from_hex(mac_hex).filter(|m| m.len() == 32).ok_or(AuthError::Malformed)?;
        self.mac(payload).verify_slice(&mac).map_err(|_| AuthError::BadSignature)?;
        // Signed, so the payload is ours; parse it.
        let mut parts = payload.splitn(3, '.');
        let (Some(VERSION), Some(expires), Some(label)) = (parts.next(), parts.next(), parts.next()) else {
            return Err(AuthError::Malformed);
        };
        let expires: u64 = expires.parse().map_err(|_| AuthError::Malformed)?;
        if unix(now) >= expires {
            return Err(AuthError::Expired);
        }
        Ok(TokenInfo { label: label.to_string(), expires })
    }

    fn mac(&self, payload: &str) -> HmacSha256 {
        let mut mac = <HmacSha256 as KeyInit>::new_from_slice(&self.secret).expect("HMAC accepts any key length");
        mac.update(payload.as_bytes());
        mac
    }
}

/// The token a WebSocket handshake carries: `Authorization: Bearer <token>`,
/// or `?token=<token>` in the URL (browsers cannot set headers on a
/// WebSocket).
pub fn token_from_request<B>(req: &tokio_tungstenite::tungstenite::http::Request<B>) -> Option<String> {
    let header = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
        .map(|t| t.trim().to_string());
    header.or_else(|| {
        req.uri()
            .query()?
            .split('&')
            .find_map(|kv| kv.strip_prefix("token="))
            .map(|t| t.to_string())
    })
}

fn unix(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

/// Creates `path` readable by this user only (0600 on Unix).
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: Duration = Duration::from_secs(86_400);

    #[test]
    fn issued_tokens_verify_until_they_expire() {
        let auth = RemoteAuth::from_secret([7; 32]);
        let now = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let token = auth.issue("Stage Left iPad!", DAY, now);
        assert!(token.starts_with("v1.1800086400.StageLeftiPad."), "{token}");
        assert_eq!(auth.verify(&token, now), Ok(TokenInfo { label: "StageLeftiPad".into(), expires: 1_800_086_400 }));
        assert_eq!(auth.verify(&token, now + DAY), Err(AuthError::Expired));
    }

    #[test]
    fn forged_and_foreign_tokens_are_rejected() {
        let auth = RemoteAuth::from_secret([7; 32]);
        let other = RemoteAuth::from_secret([8; 32]);
        let now = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let token = auth.issue("ipad", DAY, now);
        assert_eq!(other.verify(&token, now), Err(AuthError::BadSignature), "another show computer's token");
        // Extending the expiry breaks the signature.
        let forged = token.replacen("1800086400", "1900086400", 1);
        assert_eq!(auth.verify(&forged, now), Err(AuthError::BadSignature));
        let (payload, mac) = token.rsplit_once('.').unwrap();
        let flipped = format!("{payload}.{}{}", if mac.starts_with('0') { '1' } else { '0' }, &mac[1..]);
        assert_eq!(auth.verify(&flipped, now), Err(AuthError::BadSignature));
        for bad in ["", "   ", "v1", "v1.123.label", "v1.123.label.zz", &format!("{payload}.{}", &mac[..10])] {
            assert!(auth.verify(bad, now).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn key_file_is_created_private_reloaded_and_rotated() {
        let dir = std::env::temp_dir().join(format!("livestage-auth-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("remote.key");
        let now = SystemTime::now();
        let first = RemoteAuth::load_or_create(&path).unwrap();
        let token = first.issue("ipad", DAY, now);
        assert!(RemoteAuth::load_or_create(&path).unwrap().verify(&token, now).is_ok(), "same key after a restart");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let rotated = RemoteAuth::rotate(&path).unwrap();
        assert_eq!(rotated.verify(&token, now), Err(AuthError::BadSignature), "rotation revokes old tokens");
        std::fs::write(&path, "not hex").unwrap();
        assert!(RemoteAuth::load_or_create(&path).is_err(), "a damaged key is an error, not a silent new key");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!format!("{rotated:?}").contains('7'), "Debug never prints the secret");
    }

    #[test]
    fn token_is_read_from_header_or_query() {
        use tokio_tungstenite::tungstenite::http::Request;
        let req = Request::builder().uri("/").header("Authorization", "Bearer abc.def").body(()).unwrap();
        assert_eq!(token_from_request(&req).as_deref(), Some("abc.def"));
        let req = Request::builder().uri("/remote?x=1&token=v1.2.l.ff").body(()).unwrap();
        assert_eq!(token_from_request(&req).as_deref(), Some("v1.2.l.ff"));
        let req = Request::builder().uri("/").body(()).unwrap();
        assert_eq!(token_from_request(&req), None);
    }
}
