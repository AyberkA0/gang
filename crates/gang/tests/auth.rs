//! P2: GitHub sign-in against a fake GitHub, and the credential helper.

mod common;

use std::io::{BufRead, BufReader, Read, Write};

use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use common::{GANG, Sandbox};

/// A tiny HTTP server playing GitHub's Device Flow and `/user`.
fn fake_github() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    let polls = Arc::new(AtomicUsize::new(0));
    let counter = polls.clone();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            let mut length = 0;
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                if header == "\r\n" || header.is_empty() {
                    break;
                }
                if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let path = request_line.split_whitespace().nth(1).unwrap_or("");
            let json = match path {
                "/login/device/code" => r#"{"device_code":"dev123","user_code":"ABCD-1234","verification_uri":"https://github.com/login/device","interval":1,"expires_in":60}"#.to_owned(),
                "/login/oauth/access_token" => {
                    if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                        r#"{"error":"authorization_pending"}"#.to_owned()
                    } else {
                        r#"{"access_token":"gho_testtoken","token_type":"bearer"}"#.to_owned()
                    }
                }
                "/user" => r#"{"login":"tester","id":4242,"name":"Test Er"}"#.to_owned(),
                _ => "{}".to_owned(),
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-OAuth-Scopes: repo, read:user\r\nDate: Fri, 17 Oct 2026 14:32:10 GMT\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                json.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        }
    });
    (address, polls)
}

#[test]
fn login_stores_the_token_and_git_gets_it() {
    let sandbox = Sandbox::new();
    let (github, polls) = fake_github();
    let run = |args: &[&str]| {
        let output = sandbox
            .command(GANG)
            .env("GANG_GITHUB_CLIENT_ID", "test-client")
            .env("GANG_GITHUB_WEB", &github)
            .env("GANG_GITHUB_API", &github)
            .args(args)
            .output()
            .unwrap();
        common::Outcome::new(output, &sandbox.repo)
    };

    let login = run(&["auth", "login"]);
    assert_eq!(login.code, Some(0), "{login:?}");
    assert!(login.stdout.contains("ABCD-1234"), "{login:?}");
    assert!(login.stdout.contains("Logged in as tester"), "{login:?}");
    assert!(
        polls.load(Ordering::SeqCst) >= 2,
        "waited while authorization was pending"
    );

    let status = run(&["auth", "status"]);
    assert!(
        status.stdout.contains("Signed in to GitHub as tester"),
        "{status:?}"
    );

    // git asks GANG for github.com credentials, and only for those.
    let mut child = sandbox
        .command(GANG)
        .args(["credential", "get"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"protocol=https\nhost=github.com\n\n")
        .unwrap();
    let answer = String::from_utf8(child.wait_with_output().unwrap().stdout).unwrap();
    assert_eq!(answer, "username=tester\npassword=gho_testtoken\n");

    let helper = sandbox.run(
        "git",
        &[
            "config",
            "--get-all",
            "credential.https://github.com.helper",
        ],
    );
    assert!(helper.stdout.contains("credential"), "{helper:?}");

    assert_eq!(run(&["auth", "logout"]).code, Some(0));
    assert_eq!(run(&["auth", "status"]).code, Some(1));
}

#[test]
fn credential_helper_ignores_other_hosts() {
    let sandbox = Sandbox::new();
    std::fs::write(
        sandbox.home.join("gang-credentials.json"),
        r#"{"login":"tester","token":"t"}"#,
    )
    .unwrap();
    let mut child = sandbox
        .command(GANG)
        .args(["credential", "get"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"protocol=https\nhost=gitlab.com\n\n")
        .unwrap();
    assert!(child.wait_with_output().unwrap().stdout.is_empty());
}

#[test]
fn login_without_an_app_explains_what_to_do() {
    let sandbox = Sandbox::new();
    let login = sandbox
        .command(GANG)
        .env_remove("GANG_GITHUB_CLIENT_ID")
        .args(["auth", "login"])
        .output()
        .unwrap();
    assert_eq!(login.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&login.stderr).contains("GANG_GITHUB_CLIENT_ID"));
}
