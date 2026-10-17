//! `gang auth`, `gang credential` and `gang init --github` (DESIGN.md §7).

use std::ffi::OsString;
use std::io::Read;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use gang_core::auth::{self, Credentials, Store};
use gang_core::invocation::Invocation;
use gang_core::repo::Repo;
use gang_core::{clock, setup};

use crate::ui;

/// Sign in to GitHub
#[derive(Parser)]
#[command(name = "gang auth")]
pub struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Sign in with a code on github.com
    Login,
    /// Forget the GitHub token
    Logout,
    /// Show who is signed in
    Status,
}

pub fn run(invocation: &Invocation) -> Result<i32> {
    let args = Args::parse_from(
        std::iter::once(OsString::from("gang auth")).chain(invocation.args.iter().cloned()),
    );
    match args.command {
        Command::Login => login(invocation),
        Command::Logout => logout(invocation),
        Command::Status => status(),
    }
}

/// Signs in when needed; returns the credentials.
pub fn ensure_login(invocation: &Invocation) -> Result<Credentials> {
    if let Some((credentials, _)) = auth::load() {
        return Ok(credentials);
    }
    login(invocation)?;
    auth::load()
        .map(|(c, _)| c)
        .context("sign-in did not finish")
}

fn login(invocation: &Invocation) -> Result<i32> {
    if let Some((credentials, _)) = auth::load() {
        if auth::check_token(&credentials.token).is_ok() {
            ui::done(format!("Already signed in as {}", credentials.login));
            return Ok(0);
        }
    }
    let Some(client_id) = auth::client_id() else {
        bail!(
            "GANG's GitHub app is not set up in this build. Set GANG_GITHUB_CLIENT_ID to the client ID \
             of a GitHub OAuth App with Device Flow enabled, or keep using your existing git credentials."
        );
    };
    let code = auth::start_device_flow(&client_id)?;
    println!("! First copy your one-time code: {}", code.user_code);
    println!("  Open {} and enter the code.", code.verification_uri);
    print!("  Waiting for authorization...");
    use std::io::Write;
    std::io::stdout().flush()?;
    let token = auth::wait_for_token(&client_id, &code)?;
    let check = auth::check_token(&token)?;
    let store = auth::save(&Credentials {
        login: check.user.login.clone(),
        token,
    })?;
    println!(" ✓ Logged in as {}", check.user.login);
    if store == Store::File {
        ui::warn(
            "The OS keychain is not available; the token is stored in GANG's config folder, readable only by you.",
        );
    }
    if let Some(repo) = Repo::discover(&invocation.git_globals)? {
        auth::install_helper(&repo, &setup::gang_command()?)?;
        fill_identity(&repo, &check.user)?;
    }
    warn_skew(check.skew);
    Ok(0)
}

/// Sets `user.name` / `user.email` in the repository when git has none.
fn fill_identity(repo: &Repo, user: &auth::User) -> Result<()> {
    if repo.config_get("user.email")?.is_none() {
        repo.config_set(
            "user.email",
            &format!("{}+{}@users.noreply.github.com", user.id, user.login),
        )?;
    }
    if repo.config_get("user.name")?.is_none() {
        repo.config_set("user.name", user.name.as_deref().unwrap_or(&user.login))?;
    }
    Ok(())
}

fn warn_skew(skew: Option<i64>) {
    if let Some(skew) = skew.filter(|s| s.abs() > 30) {
        let direction = if skew > 0 { "ahead" } else { "behind" };
        ui::warn(format!(
            "Your computer's clock is {} {direction} of GitHub's; teammates' times may look wrong.",
            clock::ago(skew.abs()).replace(" ago", "")
        ));
    }
}

fn logout(invocation: &Invocation) -> Result<i32> {
    auth::forget()?;
    if let Some(repo) = Repo::discover(&invocation.git_globals)? {
        auth::remove_helper(&repo)?;
    }
    ui::done("Signed out of GitHub.");
    Ok(0)
}

fn status() -> Result<i32> {
    let Some((credentials, store)) = auth::load() else {
        println!("Not signed in. Run `gang auth login`.");
        return Ok(1);
    };
    let place = if store == Store::Keychain {
        "OS keychain"
    } else {
        "GANG's config folder"
    };
    match auth::check_token(&credentials.token) {
        Ok(check) => {
            ui::done(format!(
                "Signed in to GitHub as {} (token in the {place})",
                check.user.login
            ));
            if !check.scopes.is_empty() {
                println!("  Scopes: {}", check.scopes);
            }
            warn_skew(check.skew);
            Ok(0)
        }
        Err(err) => {
            eprintln!("✗ {err:#}");
            Ok(1)
        }
    }
}

/// `gang credential <get|store|erase>`: git's credential helper protocol.
pub fn credential(invocation: &Invocation) -> Result<i32> {
    let operation = invocation
        .args
        .first()
        .map(|a| a.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    if let Some(answer) = auth::helper_answer(&operation, &input) {
        print!("{answer}");
    }
    Ok(0)
}

/// `gang init --github [--public] [--name <name>]`: after setting up, creates
/// the GitHub repository and connects it as `origin`.
pub fn create_github_repo(
    invocation: &Invocation,
    repo: &Repo,
    name: Option<String>,
    public: bool,
) -> Result<()> {
    let credentials = ensure_login(invocation)?;
    let name = match name {
        Some(name) => name,
        None => repo
            .root()
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .context("cannot tell the repository's name; use --name")?,
    };
    let url = auth::create_repo(&credentials.token, &name, !public)?;
    if repo.try_run(["remote", "get-url", "origin"])?.is_some() {
        repo.run(["remote", "set-url", "origin", &url])?;
    } else {
        repo.run(["remote", "add", "origin", &url])?;
    }
    auth::install_helper(repo, &setup::gang_command()?)?;
    ui::done(format!(
        "Created {}github.com/{}/{name} and set it as origin",
        if public { "" } else { "private " },
        credentials.login
    ));
    Ok(())
}
