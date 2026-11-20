//! Sign-in with GitHub or GitLab: the OAuth 2.0 authorization-code flow.
//!
//! The app never sees a password and asks only for the user's identity (`read:user` on GitHub,
//! `read_user` on GitLab). A random `state` ties the callback to the browser that started it;
//! GitLab also gets PKCE (S256). The provider's access token is used once, to read who the user
//! is, and then dropped: nothing is stored but the user's id, login and name.
//!
//! Blocking HTTP (ureq): call these from `tokio::task::spawn_blocking`.

use crate::config::OAuthApp;
use base64::Engine;
use serde_json::Value;
use sha2::Digest;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    GitHub,
    GitLab,
}

impl Provider {
    pub fn parse(s: &str) -> Option<Provider> {
        match s {
            "github" => Some(Provider::GitHub),
            "gitlab" => Some(Provider::GitLab),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Provider::GitHub => "github",
            Provider::GitLab => "gitlab",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Provider::GitHub => "GitHub",
            Provider::GitLab => "GitLab",
        }
    }
}

/// Who signed in. `id` is stable across renames: `github:<numeric id>` or `gitlab:<numeric id>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub id: String,
    pub login: String,
    pub name: String,
    /// The address the provider shares (GitHub: the public one, if set), for notifications.
    pub email: String,
}

/// What the browser keeps (in a short-lived, HttpOnly cookie) between the redirect to the
/// provider and the callback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub provider: Provider,
    pub state: String,
    pub verifier: String,
}

impl Pending {
    pub fn new(provider: Provider) -> anyhow::Result<Pending> {
        Ok(Pending { provider, state: random_hex(32)?, verifier: random_hex(48)? })
    }

    pub fn encode(&self) -> String {
        format!("{}.{}.{}", self.provider.name(), self.state, self.verifier)
    }

    pub fn decode(s: &str) -> Option<Pending> {
        let mut it = s.split('.');
        let provider = Provider::parse(it.next()?)?;
        let state = it.next()?.to_string();
        let verifier = it.next()?.to_string();
        (it.next().is_none() && state.len() == 64 && verifier.len() == 96).then_some(Pending { provider, state, verifier })
    }
}

pub fn random_hex(bytes: usize) -> anyhow::Result<String> {
    let mut b = vec![0u8; bytes];
    getrandom::fill(&mut b).map_err(|e| anyhow::anyhow!("no randomness: {e}"))?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// Compare two secrets in time that does not depend on where they differ.
pub fn same_secret(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn pkce_challenge(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier.as_bytes()))
}

fn base(app: &OAuthApp, p: Provider) -> String {
    app.base_url.clone().unwrap_or_else(|| match p {
        Provider::GitHub => "https://github.com".into(),
        Provider::GitLab => "https://gitlab.com".into(),
    })
    .trim_end_matches('/')
    .to_string()
}

fn enc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn redirect_uri(public_url: &str, p: Provider) -> String {
    format!("{public_url}/auth/{}/callback", p.name())
}

/// Where to send the browser to sign in.
pub fn authorize_url(app: &OAuthApp, public_url: &str, pending: &Pending) -> String {
    let p = pending.provider;
    let redirect = enc(&redirect_uri(public_url, p));
    match p {
        Provider::GitHub => format!(
            "{}/login/oauth/authorize?client_id={}&redirect_uri={redirect}&scope=read%3Auser&state={}&allow_signup=false",
            base(app, p),
            enc(&app.client_id),
            pending.state
        ),
        Provider::GitLab => format!(
            "{}/oauth/authorize?client_id={}&redirect_uri={redirect}&response_type=code&scope=read_user&state={}&code_challenge={}&code_challenge_method=S256",
            base(app, p),
            enc(&app.client_id),
            pending.state,
            pkce_challenge(&pending.verifier)
        ),
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(20))).http_status_as_error(false).build().into()
}

/// Trade the callback's `code` for an access token, read the user, drop the token.
pub fn identify(app: &OAuthApp, public_url: &str, pending: &Pending, code: &str) -> anyhow::Result<Identity> {
    let p = pending.provider;
    let a = agent();
    let redirect = redirect_uri(public_url, p);
    let mut form = vec![
        ("client_id", app.client_id.as_str()),
        ("client_secret", app.client_secret.as_str()),
        ("code", code),
        ("redirect_uri", redirect.as_str()),
    ];
    let token_url = match p {
        Provider::GitHub => format!("{}/login/oauth/access_token", base(app, p)),
        Provider::GitLab => {
            form.push(("grant_type", "authorization_code"));
            form.push(("code_verifier", pending.verifier.as_str()));
            format!("{}/oauth/token", base(app, p))
        }
    };
    let mut resp = a.post(&token_url).header("Accept", "application/json").send_form(form)?;
    let status = resp.status().as_u16();
    let body: Value = resp.body_mut().read_json().map_err(|e| anyhow::anyhow!("{} token endpoint: HTTP {status}: {e}", p.label()))?;
    let token = body["access_token"].as_str().ok_or_else(|| {
        anyhow::anyhow!("{} refused the sign-in: {}", p.label(), body["error_description"].as_str().or(body["error"].as_str()).unwrap_or("no access token"))
    })?;
    let user_url = match p {
        Provider::GitHub => format!("{}/user", app.api_url.clone().unwrap_or_else(|| "https://api.github.com".into()).trim_end_matches('/')),
        Provider::GitLab => format!("{}/api/v4/user", base(app, p)),
    };
    let mut resp = a.get(&user_url).header("Authorization", format!("Bearer {token}")).header("Accept", "application/json").header("User-Agent", "egm-team").call()?;
    let status = resp.status().as_u16();
    anyhow::ensure!(status == 200, "{} user endpoint: HTTP {status}", p.label());
    let u: Value = resp.body_mut().read_json()?;
    let id = u["id"].as_u64().ok_or_else(|| anyhow::anyhow!("{} returned no user id", p.label()))?;
    let login = match p {
        Provider::GitHub => u["login"].as_str(),
        Provider::GitLab => u["username"].as_str(),
    }
    .unwrap_or_default()
    .to_string();
    let name = u["name"].as_str().filter(|s| !s.is_empty()).unwrap_or(&login).to_string();
    let email = u["email"].as_str().unwrap_or_default().to_string();
    Ok(Identity { id: format!("{}:{id}", p.name()), login, name, email })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_round_trips_and_rejects_junk() {
        let p = Pending::new(Provider::GitLab).unwrap();
        assert_eq!(Pending::decode(&p.encode()), Some(p.clone()));
        assert_eq!(Pending::decode("gitlab.short.short"), None);
        assert_eq!(Pending::decode(&format!("bitbucket.{}.{}", p.state, p.verifier)), None);
        assert_eq!(Pending::decode(&format!("{}.extra", p.encode())), None);
    }

    #[test]
    fn authorize_urls() {
        let app = OAuthApp { client_id: "id 1".into(), base_url: None, api_url: None, client_secret: String::new() };
        let p = Pending { provider: Provider::GitHub, state: "a".repeat(64), verifier: "b".repeat(96) };
        let u = authorize_url(&app, "https://egm.example", &p);
        assert!(u.starts_with("https://github.com/login/oauth/authorize?client_id=id%201&redirect_uri=https%3A%2F%2Fegm.example%2Fauth%2Fgithub%2Fcallback"), "{u}");
        assert!(u.contains("scope=read%3Auser") && u.contains(&"a".repeat(64)) && !u.contains("code_challenge"));
        let g = Pending { provider: Provider::GitLab, ..p };
        let u = authorize_url(&OAuthApp { base_url: Some("https://git.example/".into()), ..app }, "https://egm.example", &g);
        assert!(u.starts_with("https://git.example/oauth/authorize?") && u.contains("code_challenge_method=S256") && u.contains("scope=read_user"), "{u}");
    }

    #[test]
    fn pkce_matches_rfc7636_example() {
        // RFC 7636 appendix B
        assert_eq!(pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn secrets_compare() {
        assert!(same_secret("abc", "abc") && !same_secret("abc", "abd") && !same_secret("abc", "ab"));
    }
}
