//! This fork contacts no outside service. These tests fail the build if an
//! HTTP, TLS or telemetry library comes back into git-ai's dependency tree
//! (an upstream merge, a new dependency pulling one in), or if git-ai's own
//! code opens a network connection.
//!
//! git itself still pushes and fetches `refs/notes/ai` with the user's own
//! remotes; that runs through the `git` binary, not through this crate.

use std::path::Path;
use std::process::Command;

/// Crates that can talk to the network or report telemetry.
const FORBIDDEN: &[&str] = &[
    "ureq",
    "reqwest",
    "hyper",
    "h2",
    "h3",
    "isahc",
    "curl",
    "curl-sys",
    "surf",
    "attohttpc",
    "minreq",
    "ehttp",
    "native-tls",
    "openssl",
    "openssl-sys",
    "rustls",
    "tokio-rustls",
    "tokio-native-tls",
    "webpki",
    "webpki-roots",
    "tungstenite",
    "tokio-tungstenite",
    "sentry",
    "sentry-core",
    "posthog-rs",
    "opentelemetry",
    "opentelemetry-otlp",
];

#[test]
fn dependency_tree_has_no_network_or_telemetry_crates() {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let output = Command::new(cargo)
        .args([
            "tree",
            "--edges",
            "normal,build",
            "--prefix",
            "none",
            "--package",
            "git-ai",
            "--target",
            "all",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo tree runs");
    assert!(
        output.status.success(),
        "cargo tree failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let tree = String::from_utf8_lossy(&output.stdout);
    let found: Vec<&str> = tree
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| FORBIDDEN.contains(name))
        .collect();
    assert!(
        found.is_empty(),
        "network/telemetry crates in git-ai's dependency tree: {found:?}"
    );
}

#[test]
fn source_opens_no_network_connections() {
    let mut offenders = Vec::new();
    scan(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut offenders);
    assert!(
        offenders.is_empty(),
        "git-ai source opens network connections: {offenders:?}"
    );
}

fn scan(dir: &Path, offenders: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).expect("read src").flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan(&path, offenders);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let text = std::fs::read_to_string(&path).expect("read source");
            for needle in ["TcpStream", "TcpListener", "UdpSocket", "tokio::net::"] {
                if text.contains(needle) {
                    offenders.push(format!("{} uses {needle}", path.display()));
                }
            }
        }
    }
}
