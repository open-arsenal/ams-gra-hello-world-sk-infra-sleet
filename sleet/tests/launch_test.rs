//! End-to-end launch test for the default configuration.

use std::io::Write;
use std::process::Command;
use std::time::{Duration, Instant};

struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn default_config_launch_and_health_check() {
    let bin_path = env!("CARGO_BIN_EXE_sleet");

    // We run the server with the default configuration in the root workspace.
    // The working directory for the test process is `sleet/`, so we need to
    // go up one level to find `config/server.toml`.
    // Wait, the workspace root is the directory we invoke `cargo test` from,
    // but the test runner sets CWD to the crate root (`sleet/`).

    let mut server_cmd = Command::new(bin_path);
    server_cmd.current_dir("..");

    if let Ok(schema_path) = std::env::var("SLEET_SCHEMA_PATH") {
        let config_content =
            std::fs::read_to_string("../config/server.toml").expect("Failed to read server.toml");

        let config_content = config_content.replace(
            "schema_path = \"./spec/002.5.0/UCI_MessageDefinitions_v2_5_0.xsd\"",
            &format!("schema_path = \"{schema_path}\""),
        );

        let mut temp_config = tempfile::NamedTempFile::new().expect("Failed to create temp file");

        temp_config
            .write_all(config_content.as_bytes())
            .expect("Failed to write temp config");

        server_cmd.arg(temp_config.path());

        // IMPORTANT: Let temp_config live long enough to be read by the spawned process
        // In Rust, the file might get deleted if the tempfile gets dropped too early.
        // Wait, if it gets dropped here, the server process reading the config path will fail.
        // To prevent this, we'll spawn inside this block and keep temp_config alive.

        let server = server_cmd.spawn().expect("Failed to spawn sleet process");
        let _guard = ChildGuard(server);

        let start = Instant::now();
        let mut success = false;

        while start.elapsed() < Duration::from_secs(10) {
            let output = Command::new(env!("CARGO_BIN_EXE_sleet"))
                .env("SLEET_METRICS_PORT", "21312")
                .arg("--health-check")
                .output();

            if let Ok(out) = output
                && out.status.success()
            {
                success = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }

        // explicitly drop here or just let it drop at the end of block
        drop(temp_config);

        assert!(success, "Server did not become healthy within timeout");
    } else {
        server_cmd.arg("config/server.toml");

        let server = server_cmd.spawn().expect("Failed to spawn sleet process");
        let _guard = ChildGuard(server);

        let start = Instant::now();
        let mut success = false;

        while start.elapsed() < Duration::from_secs(10) {
            let output = Command::new(env!("CARGO_BIN_EXE_sleet"))
                .env("SLEET_METRICS_PORT", "21312")
                .arg("--health-check")
                .output();

            if let Ok(out) = output
                && out.status.success()
            {
                success = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }

        assert!(success, "Server did not become healthy within timeout");
    }
}
