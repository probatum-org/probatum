//! Minimal HTTP/1.1 GET over TcpStream — enough for localhost readiness and checks,
//! zero heavy dependencies in v0.

use anyhow::{bail, Context, Result};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

pub struct Response {
    pub status: u16,
    pub body: String,
}

/// `url` like `http://127.0.0.1:8080/healthz`
pub fn get(url: &str, timeout: Duration) -> Result<Response> {
    request("GET", url, None, &[], timeout)
}

pub fn request(
    method: &str,
    url: &str,
    body: Option<&str>,
    headers: &[(String, String)],
    timeout: Duration,
) -> Result<Response> {
    let rest = url
        .strip_prefix("http://")
        .context("only http:// URLs are supported in v0")?;
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let addr = if hostport.contains(':') {
        hostport.to_string()
    } else {
        format!("{hostport}:80")
    };

    // Resolve the host (DNS included) and try every address in turn: localhost
    // may resolve to ::1 while the service binds 127.0.0.1 (issue #2). A check
    // is not latency-sensitive, sequential fallback is fine.
    let addrs: Vec<SocketAddr> = addr
        .to_socket_addrs()
        .with_context(|| format!("cannot resolve {addr}"))?
        .collect();
    let mut stream = None;
    let mut last_err: Option<std::io::Error> = None;
    for a in &addrs {
        match TcpStream::connect_timeout(a, timeout) {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(e) => last_err = Some(e),
        }
    }
    let Some(stream) = stream else {
        bail!(
            "nothing answered on {} ({})",
            addrs
                .iter()
                .map(|a| a.to_string())
                .collect::<Vec<_>>()
                .join(", "),
            last_err
                .map(|e| e.to_string())
                .unwrap_or_else(|| "host resolved to no addresses".into())
        );
    };
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let mut stream = stream;

    let body = body.unwrap_or("");
    let mut extra = String::new();
    for (k, v) in headers {
        extra.push_str(&format!("{k}: {v}\r\n"));
    }
    if method == "POST" {
        extra.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {hostport}\r\nConnection: close\r\nUser-Agent: probatum/0.1\r\n{extra}\r\n{body}"
    )?;

    let mut raw = String::new();
    stream.read_to_string(&mut raw)?;

    let mut parts = raw.splitn(2, "\r\n\r\n");
    let head = parts.next().unwrap_or_default();
    let body = parts.next().unwrap_or_default().to_string();
    let status_line = head.lines().next().unwrap_or_default();
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if status == 0 {
        bail!("malformed HTTP response: {status_line}");
    }
    Ok(Response { status, body })
}
