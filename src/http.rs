//! Minimal HTTP/1.1 GET over TcpStream — enough for localhost readiness and checks,
//! zero heavy dependencies in v0.

use anyhow::{bail, Context, Result};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

pub struct Response {
    pub status: u16,
    pub body: String,
}

/// `url` like `http://127.0.0.1:8080/healthz`
pub fn get(url: &str, timeout: Duration) -> Result<Response> {
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

    let stream = TcpStream::connect_timeout(
        &addr.parse().with_context(|| format!("bad address {addr}"))?,
        timeout,
    )?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let mut stream = stream;

    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {hostport}\r\nConnection: close\r\nUser-Agent: probatum/0.1\r\n\r\n"
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
