//! OAuth 2 helpers shared by GitLab and Bitbucket: PKCE and the token
//! response.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

/// A PKCE verifier and its S256 challenge, plus a `state` value to check
/// on the redirect.
#[derive(Clone, PartialEq, Eq)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
    pub state: String,
}

impl std::fmt::Debug for Pkce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pkce")
            .field("challenge", &self.challenge)
            .finish_non_exhaustive()
    }
}

impl Pkce {
    /// A fresh random verifier and state.
    pub fn generate() -> Result<Self, String> {
        let mut bytes = [0_u8; 48];
        getrandom::fill(&mut bytes).map_err(|e| e.to_string())?;
        let verifier = URL_SAFE_NO_PAD.encode(&bytes[..32]);
        let state = URL_SAFE_NO_PAD.encode(&bytes[32..]);
        Ok(Self::from_parts(verifier, state))
    }

    #[must_use]
    pub fn from_parts(verifier: String, state: String) -> Self {
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Self {
            verifier,
            challenge,
            state,
        }
    }
}

/// Tokens from an authorization-code or refresh exchange.
#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Seconds until the access token expires.
    pub expires_in: Option<u64>,
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens")
            .field("expires_in", &self.expires_in)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s256_challenge_matches_rfc_7636() {
        // RFC 7636, appendix B.
        let pkce = Pkce::from_parts(
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".into(),
            "s".into(),
        );
        assert_eq!(
            pkce.challenge,
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert!(!format!("{pkce:?}").contains("dBjft"));
        let random = Pkce::generate().unwrap();
        assert_eq!(random.verifier.len(), 43);
        assert_ne!(random.verifier, Pkce::generate().unwrap().verifier);
    }
}
