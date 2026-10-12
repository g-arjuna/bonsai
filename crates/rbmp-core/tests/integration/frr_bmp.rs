/// FRRouting BMP integration tests (RV4-9 T1).
///
/// These tests require Docker with the `frrouting/frr` image available.
/// They are marked `#[ignore]` so they do NOT run in CI by default.
///
/// Run manually on Ubuntu:
///   cargo test -p rbmp-core --test integration frr_bmp -- --ignored --nocapture
///
/// Prerequisites:
///   docker pull frrouting/frr:9.1
///   A free port 15000 on localhost (used as ephemeral BMP listen port)
#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
    use std::time::Duration;
    use tokio::time::sleep;

    /// Allocate an ephemeral TCP port on localhost (returns the port number).
    fn free_port() -> u16 {
        TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    /// Verify Docker is available and `frrouting/frr:9.1` is pulled.
    fn docker_available() -> bool {
        std::process::Command::new("docker")
            .args(["image", "inspect", "frrouting/frr:9.1"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Smoke-test: bring up FRR container configured to send BMP to a
    /// localhost TCP listener, confirm a TCP connection arrives within 15 s.
    ///
    /// This does NOT verify full parse → DuckDB → API; it validates that
    /// the BMP session establishment works end-to-end with a real BGP speaker.
    #[tokio::test]
    #[ignore = "requires docker + frrouting/frr:9.1 image"]
    async fn frr_bmp_session_connects() {
        assert!(
            docker_available(),
            "Explicitly requested FRR test requires Docker and frrouting/frr:9.1"
        );

        let port = free_port();
        let listen_addr = format!("0.0.0.0:{port}");

        // Start a bare TCP listener to accept the BMP connection
        let listener = tokio::net::TcpListener::bind(&listen_addr).await
            .expect("bind test listener");

        let host_ip = "172.17.0.1"; // default Docker bridge host IP
        let container_name = format!("rustybmp-frr-test-{port}");

        // Launch FRR container
        let status = std::process::Command::new("docker")
            .args([
                "run", "--rm", "-d",
                "--name", &container_name,
                "--cap-add=NET_ADMIN",
                "frrouting/frr:9.1",
                "/usr/lib/frr/docker-start",
            ])
            .status()
            .expect("docker run");
        assert!(status.success(), "docker run frr failed");

        // Give FRR 3s to start daemons
        sleep(Duration::from_secs(3)).await;

        // Configure FRR via vtysh: BGP + BMP targeting host
        let vtysh_cfg = format!(
            "configure terminal\n\
             router bgp 65099\n\
              bgp router-id 192.0.2.99\n\
              bmp targets test\n\
               address {host_ip} port {port}\n\
               monitor ipv4 unicast pre-policy\n\
              !\n\
             !\n\
             end\n"
        );
        std::process::Command::new("docker")
            .args(["exec", &container_name, "vtysh", "-c", &vtysh_cfg])
            .status()
            .expect("vtysh configure");

        // Wait up to 15 s for BMP connection
        let connected = tokio::time::timeout(
            Duration::from_secs(15),
            listener.accept(),
        ).await;

        // Cleanup
        let _ = std::process::Command::new("docker")
            .args(["stop", &container_name])
            .status();

        let (stream, peer) = connected
            .expect("timed out waiting for BMP connection from FRR")
            .expect("accept error");

        println!("BMP connection from FRR: peer={peer}");
        assert_eq!(peer.ip(), IpAddr::V4(Ipv4Addr::new(172, 17, 0, 2)));
        drop(stream);
    }

    /// Full round-trip test: FRR announces 3 prefixes → rustybmp parses →
    /// verifies BMP Initiation + RouteMonitoring PDUs arrive on the socket.
    #[tokio::test]
    #[ignore = "requires docker + frrouting/frr:9.1 image"]
    async fn frr_bmp_route_monitoring_received() {
        use rbmp_core::bmp::parser::{parse_bmp_message, DEFAULT_MAX_FRAME};
        use rbmp_core::bmp::types::BmpPayload;

        assert!(
            docker_available(),
            "Explicitly requested FRR test requires Docker and frrouting/frr:9.1"
        );

        let port = free_port();
        let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}")).await.unwrap();
        let host_ip = "172.17.0.1";
        let container_name = format!("rustybmp-frr-rm-{port}");

        std::process::Command::new("docker")
            .args(["run", "--rm", "-d", "--name", &container_name,
                   "--cap-add=NET_ADMIN", "frrouting/frr:9.1", "/usr/lib/frr/docker-start"])
            .status().unwrap();
        sleep(Duration::from_secs(3)).await;

        let cfg = format!(
            "configure terminal\n\
             router bgp 65099\n\
              bgp router-id 192.0.2.99\n\
              address-family ipv4 unicast\n\
               network 203.0.113.0/24\n\
               network 198.51.100.0/24\n\
               network 192.0.2.0/24\n\
              exit-address-family\n\
              bmp targets test\n\
               address {host_ip} port {port}\n\
               monitor ipv4 unicast pre-policy\n\
              !\n\
             !\n\
             end\n"
        );
        std::process::Command::new("docker")
            .args(["exec", &container_name, "vtysh", "-c", &cfg])
            .status().unwrap();

        // Read first few BMP PDUs (up to 5 s per PDU)
        let (mut stream, _) = tokio::time::timeout(
            Duration::from_secs(15), listener.accept()
        ).await.expect("no BMP connection").unwrap();

        let speaker = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(172, 17, 0, 2)), port)
            .ip();

        let mut buf = vec![0u8; 65535];
        let mut saw_initiation    = false;
        let mut saw_route_monitor = false;

        for _ in 0..20 {
            use tokio::io::AsyncReadExt;
            let n = tokio::time::timeout(
                Duration::from_secs(5),
                stream.read(&mut buf),
            ).await.unwrap_or(Ok(0)).unwrap_or(0);

            if n < 6 { break; }

            if let Ok(payload) = parse_bmp_message(&buf[..n], speaker, DEFAULT_MAX_FRAME) {
                match payload {
                    BmpPayload::Initiation { .. }    => saw_initiation    = true,
                    BmpPayload::RouteMonitoring { .. } => saw_route_monitor = true,
                    _ => {}
                }
            }
            if saw_initiation && saw_route_monitor { break; }
        }

        let _ = std::process::Command::new("docker")
            .args(["stop", &container_name]).status();

        assert!(saw_initiation,    "expected BMP Initiation message from FRR");
        assert!(saw_route_monitor, "expected BMP RouteMonitoring message from FRR");
    }
}
