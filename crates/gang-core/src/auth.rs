//! GitHub sign-in (DESIGN.md §7): Device Flow, the token in the OS keychain,
//! GANG as git's credential helper, and the few GitHub API calls GANG needs.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::repo::Repo;

/// The `client_id` of GANG's GitHub OAuth App (Device Flow enabled), built
/// in by release builds (`GANG_GITHUB_CLIENT_ID` at compile time). The same
/// variable at run time overrides it.
const CLIENT_ID: Option<&str> = option_env!("GANG_GITHUB_CLIENT_ID");

const KEYRING_SERVICE: &str = "gang";
const KEYRING_USER: &str = "github";

pub fn client_id() -> Option<String> {
    std::env::var("GANG_GITHUB_CLIENT_ID")
        .ok()
        .or_else(|| CLIENT_ID.map(str::to_owned))
        .filter(|id| !id.is_empty())
}

fn web_base() -> String {
    std::env::var("GANG_GITHUB_WEB").unwrap_or_else(|_| "https://github.com".to_owned())
}

fn api_base() -> String {
    std::env::var("GANG_GITHUB_API").unwrap_or_else(|_| "https://api.github.com".to_owned())
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .http_status_as_error(false)
        .build()
        .into()
}

/// A signed-in GitHub user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credentials {
    pub login: String,
    pub token: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub interval: u64,
    pub expires_in: u64,
}

/// Step 1 of Device Flow: a code for the user to enter on GitHub.
pub fn start_device_flow(client_id: &str) -> Result<DeviceCode> {
    let mut response = agent()
        .post(format!("{}/login/device/code", web_base()))
        .header("Accept", "application/json")
        .send_form([("client_id", client_id), ("scope", "repo read:user")])
        .context("could not reach GitHub")?;
    if !response.status().is_success() {
        bail!("GitHub refused the sign-in request ({})", response.status());
    }
    Ok(response.body_mut().read_json()?)
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    error: Option<String>,
    interval: Option<u64>,
}

/// Step 2: waits until the user has approved, and returns the token.
pub fn wait_for_token(client_id: &str, code: &DeviceCode) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(code.expires_in);
    let mut interval = code.interval.max(1);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_secs(interval));
        let mut response = agent()
            .post(format!("{}/login/oauth/access_token", web_base()))
            .header("Accept", "application/json")
            .send_form([
                ("client_id", client_id),
                ("device_code", code.device_code.as_str()),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .context("could not reach GitHub")?;
        let answer: TokenResponse = response.body_mut().read_json()?;
        if let Some(token) = answer.access_token {
            return Ok(token);
        }
        match answer.error.as_deref() {
            Some("authorization_pending") => {}
            Some("slow_down") => interval = answer.interval.unwrap_or(interval + 5),
            Some("expired_token") => bail!("the code expired; run `gang auth login` again"),
            Some("access_denied") => bail!("sign-in was cancelled on GitHub"),
            Some(other) => bail!("GitHub sign-in failed: {other}"),
            None => bail!("GitHub sign-in failed"),
        }
    }
    bail!("the code expired; run `gang auth login` again")
}

#[derive(Debug, Clone, Deserialize)]
pub struct User {
    pub login: String,
    pub id: u64,
    pub name: Option<String>,
}

/// What `GET /user` says about a token.
pub struct TokenCheck {
    pub user: User,
    pub scopes: String,
    /// Our clock minus GitHub's, in seconds.
    pub skew: Option<i64>,
}

pub fn check_token(token: &str) -> Result<TokenCheck> {
    let mut response = agent()
        .get(format!("{}/user", api_base()))
        .header("Accept", "application/vnd.github+json")
        .header("Authorization", &format!("Bearer {token}"))
        .header("User-Agent", "gang")
        .call()
        .context("could not reach GitHub")?;
    if response.status().as_u16() == 401 {
        bail!("the GitHub token is no longer valid; run `gang auth login`");
    }
    if !response.status().is_success() {
        bail!("GitHub answered {}", response.status());
    }
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let scopes = header("x-oauth-scopes").unwrap_or_default();
    let skew = header("date")
        .and_then(|d| http_date(&d))
        .map(|server| crate::clock::now() - server);
    let user: User = response.body_mut().read_json()?;
    Ok(TokenCheck { user, scopes, skew })
}

/// `Fri, 17 Oct 2026 14:32:10 GMT` → seconds since the epoch.
fn http_date(text: &str) -> Option<i64> {
    let parts: Vec<&str> = text.split_whitespace().collect();
    let [_, day, month, year, time, _] = parts.as_slice() else {
        return None;
    };
    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let month = months.iter().position(|m| m == month)? + 1;
    crate::clock::parse(&format!("{year}-{month:02}-{day:0>2}T{time}Z"))
}

/// Creates a repository for the signed-in user; returns its clone URL.
pub fn create_repo(token: &str, name: &str, private: bool) -> Result<String> {
    #[derive(Deserialize)]
    struct Created {
        clone_url: String,
    }
    let mut response = agent()
        .post(format!("{}/user/repos", api_base()))
        .header("Accept", "application/vnd.github+json")
        .header("Authorization", &format!("Bearer {token}"))
        .header("User-Agent", "gang")
        .send_json(serde_json::json!({"name": name, "private": private}))
        .context("could not reach GitHub")?;
    if !response.status().is_success() {
        let body = response.body_mut().read_to_string().unwrap_or_default();
        bail!(
            "GitHub could not create {name}: {} {body}",
            response.status()
        );
    }
    let created: Created = response.body_mut().read_json()?;
    Ok(created.clone_url)
}

/// Where the token is kept when the OS keychain is not available, or when
/// `GANG_CREDENTIALS_FILE` asks for a file.
fn credentials_file() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("GANG_CREDENTIALS_FILE") {
        return Some(PathBuf::from(path));
    }
    directories::ProjectDirs::from("", "", "gang")
        .map(|dirs| dirs.config_dir().join("credentials.json"))
}

fn use_file() -> bool {
    std::env::var_os("GANG_CREDENTIALS_FILE").is_some()
}

/// Where the stored token came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Store {
    Keychain,
    File,
}

pub fn save(credentials: &Credentials) -> Result<Store> {
    let json = serde_json::to_string(credentials)?;
    if !use_file() {
        if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER) {
            if entry.set_password(&json).is_ok() {
                return Ok(Store::Keychain);
            }
        }
    }
    let path = credentials_file().context("no place to store the GitHub token")?;
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(&path, json)?;
    restrict(&path)?;
    Ok(Store::File)
}

#[cfg(unix)]
fn restrict(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict(_path: &std::path::Path) -> Result<()> {
    Ok(())
}

/// The stored credentials, if any.
pub fn load() -> Option<(Credentials, Store)> {
    if !use_file() {
        if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER) {
            if let Ok(json) = entry.get_password() {
                if let Ok(credentials) = serde_json::from_str(&json) {
                    return Some((credentials, Store::Keychain));
                }
            }
        }
    }
    let text = std::fs::read_to_string(credentials_file()?).ok()?;
    serde_json::from_str(&text).ok().map(|c| (c, Store::File))
}

pub fn forget() -> Result<()> {
    if !use_file() {
        if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER) {
            let _ = entry.delete_credential();
        }
    }
    if let Some(path) = credentials_file() {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

const HELPER_KEY: &str = "credential.https://github.com.helper";

/// Makes git ask GANG for GitHub credentials in this repository.
pub fn install_helper(repo: &Repo, gang: &str) -> Result<()> {
    let wanted = format!("!{gang} credential");
    let current = repo
        .try_run(["config", "--local", "--get-all", HELPER_KEY])?
        .unwrap_or_default();
    if current.lines().any(|l| l == wanted) {
        return Ok(());
    }
    // An empty first entry stops git from also using helpers from elsewhere.
    repo.run(["config", "--local", "--replace-all", HELPER_KEY, ""])?;
    repo.run(["config", "--local", "--add", HELPER_KEY, &wanted])?;
    Ok(())
}

pub fn remove_helper(repo: &Repo) -> Result<()> {
    let _ = repo.try_run(["config", "--local", "--unset-all", HELPER_KEY])?;
    Ok(())
}

/// Answers one request of git's credential helper protocol. `input` is what
/// git sent (`key=value` lines); returns what to print.
pub fn helper_answer(operation: &str, input: &str) -> Option<String> {
    if operation != "get" {
        return None;
    }
    let field = |name: &str| {
        input
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{name}=")).map(str::to_owned))
    };
    if field("protocol").as_deref() != Some("https")
        || field("host").as_deref() != Some("github.com")
    {
        return None;
    }
    let (credentials, _) = load()?;
    Some(format!(
        "username={}\npassword={}\n",
        credentials.login, credentials.token
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_dates() {
        assert_eq!(
            http_date("Fri, 17 Oct 2026 14:32:10 GMT"),
            crate::clock::parse("2026-10-17T14:32:10Z")
        );
        assert_eq!(http_date("garbage"), None);
    }
}
