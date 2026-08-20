//! Live smoke test for the filtered Docker gateway.

use tome_lib::egress::docker::{resolve_daemon_socket, DockerGateway, DockerPolicy};

fn tomebind_path() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("tome-docker-live-test");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("gw.sock")
}

#[tokio::test]
#[ignore]
async fn docker_version_works_through_the_gateway() {
    let Some(daemon) = resolve_daemon_socket() else {
        eprintln!("SKIP: no docker daemon socket found");
        return;
    };
    let sock = tomebind_path();
    let _ = std::fs::remove_file(&sock);
    eprintln!("gateway socket: {}", sock.display());
    let gw = DockerGateway::spawn(
        sock.clone(),
        daemon,
        DockerPolicy {
            allowed_mount_roots: vec![std::env::home_dir().unwrap_or_default()],
        },
        |d| eprintln!("DENIED: {}", d.reason),
    )
    .await
    .unwrap();

    let host = format!("unix://{}", sock.display());
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        tokio::process::Command::new("docker")
            .env("DOCKER_HOST", &host)
            .env("DOCKER_BUILDKIT", "0")
            .args(["version", "--format", "{{.Server.Version}}"])
            .output(),
    )
    .await;
    match out {
        Ok(Ok(o)) => {
            eprintln!("status: {}", o.status);
            eprintln!("stdout: {}", String::from_utf8_lossy(&o.stdout));
            eprintln!("stderr: {}", String::from_utf8_lossy(&o.stderr));
        }
        Ok(Err(e)) => eprintln!("spawn error: {e}"),
        Err(_) => eprintln!("TIMED OUT after 15s — CLI hung"),
    }
    gw.shutdown();
}
