use std::io::{Read, Write};
use std::net::TcpStream;
use std::process;

// Minimal TCP-level readiness probe. No runtime deps beyond std.
// Used as the container HEALTHCHECK instead of curl.
fn main() {
    let port = std::env::var("BONSAI_HTTP_PORT").unwrap_or_else(|_| "3000".to_string());
    let addr = format!("127.0.0.1:{port}");

    let ok = TcpStream::connect(&addr)
        .and_then(|mut s| {
            s.write_all(b"GET /api/readiness HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n")?;
            let mut buf = [0u8; 16];
            s.read_exact(&mut buf)?;
            Ok(buf.get(9..12) == Some(b"200"))
        })
        .unwrap_or(false);

    process::exit(if ok { 0 } else { 1 });
}
