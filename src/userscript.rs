//! The browser script (Tampermonkey) for files the launcher hands to the
//! visitor's browser. It clicks the free download buttons of ModsFire,
//! Sharemods, mods.to, Google Drive and authors' pages in tabs the launcher
//! opened, and never touches Cloudflare's check.
//! For installation it is served briefly on 127.0.0.1: Tampermonkey offers to
//! install any address ending in `.user.js`.

use anyhow::Result;
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    runtime::Runtime,
};

pub const SCRIPT: &str = include_str!("../assets/userscript/truck-launcher-modsfire.user.js");
const NAME: &str = "truck-launcher-modsfire.user.js";
/// Address mark the script looks for; pages without it are left alone.
pub const MARK: &str = "truck-launcher";

/// The page address with the launcher's mark, so the script acts on that tab only.
pub fn tagged(url: &str) -> String {
    let base = url.split('#').next().unwrap_or(url);
    format!("{base}#{MARK}")
}

/// Serves the script on a loopback port for ten minutes; returns its address.
pub fn serve(runtime: &Runtime) -> Result<String> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    listener.set_nonblocking(true)?;
    let address = format!("http://127.0.0.1:{}/{NAME}", listener.local_addr()?.port());
    runtime.spawn(async move {
        let Ok(listener) = tokio::net::TcpListener::from_std(listener) else {
            return;
        };
        let serving = async {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    continue;
                };
                tokio::spawn(async move {
                    let mut request = [0u8; 2048];
                    let read = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut request))
                        .await
                        .ok()
                        .and_then(Result::ok)
                        .unwrap_or(0);
                    let line = String::from_utf8_lossy(&request[..read]);
                    let response = if line.starts_with(&format!("GET /{NAME} ")) {
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/javascript; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{SCRIPT}",
                            SCRIPT.len()
                        )
                    } else {
                        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()
                    };
                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.shutdown().await;
                });
            }
        };
        let _ = tokio::time::timeout(Duration::from_secs(600), serving).await;
    });
    Ok(address)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mark_replaces_any_fragment_and_the_script_requires_it() {
        assert_eq!(
            tagged("https://modsfire.com/f69b7lEnUu3pT4y"),
            "https://modsfire.com/f69b7lEnUu3pT4y#truck-launcher"
        );
        assert_eq!(
            tagged("https://modsfire.com/abc#old"),
            "https://modsfire.com/abc#truck-launcher"
        );
        assert!(SCRIPT.starts_with("// ==UserScript=="));
        assert!(SCRIPT.contains(&format!("const MARK = '{MARK}'")));
        // The version shown in the tab title follows the header.
        let header = SCRIPT
            .lines()
            .find_map(|l| l.strip_prefix("// @version"))
            .unwrap()
            .trim();
        let shown = SCRIPT
            .split("const VERSION = '")
            .nth(1)
            .and_then(|r| r.split('\'').next())
            .unwrap();
        assert!(
            header.starts_with(&format!("{shown}.")),
            "{header} vs {shown}"
        );
        // The Cloudflare widget is only waited for, never clicked.
        assert!(!SCRIPT.contains("widget.click"));
    }

    #[test]
    fn the_install_address_serves_only_the_script() {
        let runtime = Runtime::new().unwrap();
        let address = serve(&runtime).unwrap();
        let (body, missing) = runtime.block_on(async {
            let client = reqwest::Client::builder().no_proxy().build().unwrap();
            let body = client
                .get(&address)
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap();
            let missing = client
                .get(address.replace(NAME, "other.js"))
                .send()
                .await
                .unwrap()
                .status();
            (body, missing)
        });
        assert_eq!(body, SCRIPT);
        assert_eq!(missing, reqwest::StatusCode::NOT_FOUND);
    }
}
