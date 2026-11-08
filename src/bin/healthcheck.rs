use std::io::{Read, Write};
use std::net::TcpStream;
use std::process;

// Minimal TCP-level readiness probe. No runtime deps beyond std.
// Used as the container HEALTHCHECK instead of curl.
//
// Env overrides:
//   BONSAI_HTTP_PORT          — port to probe (default 3000)
//   BONSAI_HEALTHCHECK_PATH   — HTTP path (default /readyz; bonsai-routing
//                               uses /internal/routing/v1/health on :8081)
//   BONSAI_HEALTHCHECK_TOKEN  — optional token sent as x-bonsai-routing-token
fn main() {
    let port = std::env::var("BONSAI_HTTP_PORT").unwrap_or_else(|_| "3000".to_string());
    let path = std::env::var("BONSAI_HEALTHCHECK_PATH").unwrap_or_else(|_| "/readyz".to_string());
    let token = std::env::var("BONSAI_HEALTHCHECK_TOKEN").unwrap_or_default();
    let addr = format!("127.0.0.1:{port}");

    let mut request = format!("GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\n");
    if !token.is_empty() {
        request.push_str(&format!("x-bonsai-routing-token: {token}\r\n"));
    }
    request.push_str("\r\n");

    let ok = TcpStream::connect(&addr)
        .and_then(|mut s| {
            s.write_all(request.as_bytes())?;
            let mut buf = [0u8; 16];
            s.read_exact(&mut buf)?;
            Ok(buf.get(9..12) == Some(b"200"))
        })
        .unwrap_or(false);

    process::exit(if ok { 0 } else { 1 });
}
