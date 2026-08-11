use super::session::{
    apk_json_path, get_session_artifacts, opkg_json_path, sops_key_file, ssh_key_path,
};
use nuci::deploy::{self, DeployConfig, RealSsh};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use testcontainers::ImageExt;
use testcontainers::runners::AsyncRunner;

/// A running OpenWrt test container (opkg or apk).
///
/// The container is started via testcontainers-rs for lifecycle management
/// (auto-cleanup on drop), but shell/UCI commands are executed via the
/// docker/podman CLI since testcontainers-rs v0.28 does not expose exec.
pub struct Target {
    pub role: String,
    pub host: String,
    pub port: u16,
    /// Container name (used for CLI exec calls).
    pub name: String,
    /// The testcontainers handle — dropping it stops/removes the container.
    pub _container: Option<testcontainers::ContainerAsync<testcontainers::GenericImage>>,
}

impl Target {
    /// Spawn a new container for the given role ("opkg" or "apk").
    ///
    /// Returns `None` if the container cannot be started.
    pub async fn new(role: &str) -> Option<Self> {
        let engine = std::env::var("CONTAINER_ENGINE").unwrap_or_else(|_| "podman".to_string());
        let docker_host = std::env::var("DOCKER_HOST").unwrap_or_else(|_| {
            if engine == "podman" {
                "unix:///run/podman/podman.sock".to_string()
            } else {
                "unix:///var/run/docker.sock".to_string()
            }
        });
        unsafe {
            std::env::set_var("DOCKER_HOST", &docker_host);
        }

        let image_name = match role {
            "opkg" => "openwrt-test-opkg-env",
            "apk" => "openwrt-test-apk-env",
            "agent" => "openwrt-agent-test-env",
            _ => panic!("unknown role: {role}"),
        };

        if !ensure_image(&engine, image_name, role) {
            return None;
        }

        let name = format!("nuci-{role}-{}", uuid::Uuid::new_v4());
        let image = testcontainers::GenericImage::new(image_name, "latest")
            .with_exposed_port(testcontainers::core::ContainerPort::Tcp(22))
            .with_container_name(&name);

        let container = image.start().await.ok()?;

        let host = container.get_host().await.ok()?;
        let port = container
            .get_host_port_ipv4(testcontainers::core::ContainerPort::Tcp(22))
            .await
            .ok()?;

        // Inject SSH key (skip for agent role).
        if role != "agent" {
            inject_ssh_key(&engine, &name, &ssh_key_path()).await;
        }

        // Wait for SSH to be ready.
        if !wait_for_ssh(port, Duration::from_secs(30)) {
            return None;
        }

        Some(Self {
            role: role.to_string(),
            host: host.to_string(),
            port,
            name,
            _container: Some(container),
        })
    }

    /// Run a shell command inside the container.
    pub fn sh(&self, cmd: &str) -> String {
        let engine = detect_engine();
        let output = Command::new(engine)
            .args(["exec", &self.name, "sh", "-c", cmd])
            .output()
            .expect("failed to exec in container");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// Run a command and return true if it succeeds.
    pub fn sh_ok(&self, cmd: &str) -> bool {
        let engine = detect_engine();
        Command::new(engine)
            .args(["exec", &self.name, "sh", "-c", cmd])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Get a UCI value from the container.
    pub fn uci_get(&self, path: &str) -> Option<String> {
        let out = self.sh(&format!("uci get {path}"));
        if out.is_empty() { None } else { Some(out) }
    }

    /// Check if a UCI path exists.
    pub fn uci_exists(&self, path: &str) -> bool {
        self.sh_ok(&format!("uci get {path}"))
    }

    /// Spawn SSH to the container with the test SSH key.
    #[allow(dead_code)]
    pub fn ssh_cmd(&self, cmd: &str) -> String {
        let output = Command::new("ssh")
            .args([
                "-o",
                "StrictHostKeyChecking=no",
                "-o",
                "BatchMode=yes",
                "-o",
                "UserKnownHostsFile=/dev/null",
                "-o",
                "ConnectTimeout=10",
                "-p",
                &self.port.to_string(),
                "-i",
                ssh_key_path().to_str().unwrap(),
                &format!("root@{}", self.host),
                cmd,
            ])
            .output()
            .expect("failed to run ssh");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    pub fn ssh_ok(&self, cmd: &str) -> bool {
        Command::new("ssh")
            .args([
                "-o",
                "StrictHostKeyChecking=no",
                "-o",
                "BatchMode=yes",
                "-o",
                "UserKnownHostsFile=/dev/null",
                "-o",
                "ConnectTimeout=10",
                "-p",
                &self.port.to_string(),
                "-i",
                ssh_key_path().to_str().unwrap(),
                &format!("root@{}", self.host),
                cmd,
            ])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Wait for SSH to become available (polling).
    pub fn wait_ssh(&self, timeout: Duration) -> bool {
        let start = std::time::Instant::now();
        while start.elapsed() < timeout {
            if self.ssh_ok("echo ok") {
                return true;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        false
    }

    /// Deploy the appropriate test config to this target with --force.
    pub fn nuci_deploy(&self, json_path: &Path) {
        let artifacts = get_session_artifacts();
        let config = DeployConfig {
            port: self.port,
            identity_file: Some(artifacts.ssh_key.to_string_lossy().into()),
            force: true,
            no_sops: false,
            watchdog_timeout: 10,
        };
        let target = format!("root@{}", self.host);
        unsafe {
            std::env::set_var("SOPS_AGE_KEY_FILE", sops_key_file().to_str().unwrap());
        }
        let _ = deploy::run(json_path, &target, &config, None, &RealSsh);
    }

    /// Reset UCI state by redeploying with --force (idempotent).
    pub fn reset_uci_state(&self) {
        let json = if self.role == "opkg" {
            opkg_json_path()
        } else {
            apk_json_path()
        };
        self.nuci_deploy(&json);
        restore_reload_config(self);
    }
}

fn detect_engine() -> &'static str {
    static ENGINE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ENGINE.get_or_init(|| {
        std::env::var("CONTAINER_ENGINE").unwrap_or_else(|_| {
            if Command::new("podman")
                .args(["ps"])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
            {
                "podman".to_string()
            } else {
                "docker".to_string()
            }
        })
    })
}

/// The reload_config mock script baked into the containerfiles.
const RELOAD_CONFIG_ORIGINAL: &str = "#!/bin/sh\n\
     for s in /etc/init.d/*; do\n\
     case \"$s\" in\n\
       /etc/init.d/dropbear|/etc/init.d/nuci_rollback) ;;\n\
       *) [ -x \"$s\" ] && \"$s\" reload >/dev/null 2>&1 ;;\n\
     esac\n\
     done\n\
     exit 0\n";

fn restore_reload_config(target: &Target) {
    target.sh(&format!(
        "cat > /sbin/reload_config <<'EOF'\n{}\nEOF\nchmod +x /sbin/reload_config",
        RELOAD_CONFIG_ORIGINAL
    ));
}

fn ensure_image(engine: &str, image_name: &str, role: &str) -> bool {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let exists = Command::new(engine)
        .args(["image", "inspect", image_name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if exists {
        return true;
    }

    let containerfile = match role {
        "opkg" => "tests/Containerfile.opkg",
        "apk" => "tests/Containerfile.apk",
        "agent" => "tests/Containerfile.agent-test",
        _ => return false,
    };
    Command::new(engine)
        .args([
            "build",
            "-q",
            "-t",
            image_name,
            "-f",
            manifest_dir.join(containerfile).to_str().unwrap(),
            manifest_dir.to_str().unwrap(),
        ])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

async fn inject_ssh_key(engine: &str, name: &str, ssh_key: &Path) {
    let pub_key = fs::read_to_string(ssh_key.with_extension("pub")).expect("read public key");
    let full_cmd = format!(
        "mkdir -p /etc/dropbear && cat > /etc/dropbear/authorized_keys <<'KEYEOF'\n{}\nKEYEOF",
        pub_key.trim()
    );
    Command::new(engine)
        .args(["exec", name, "sh", "-c", &full_cmd])
        .status()
        .ok();
    Command::new(engine)
        .args(["exec", name, "chmod", "700", "/etc/dropbear"])
        .status()
        .ok();
    Command::new(engine)
        .args([
            "exec",
            name,
            "chmod",
            "600",
            "/etc/dropbear/authorized_keys",
        ])
        .status()
        .ok();
}

fn wait_for_ssh(port: u16, timeout: Duration) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        let ok = Command::new("ssh")
            .args([
                "-o",
                "StrictHostKeyChecking=no",
                "-o",
                "BatchMode=yes",
                "-o",
                "UserKnownHostsFile=/dev/null",
                "-o",
                "ConnectTimeout=3",
                "-p",
                &port.to_string(),
                "root@127.0.0.1",
                "echo ok",
            ])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            return true;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    false
}
