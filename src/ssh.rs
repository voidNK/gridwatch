//! Thin wrapper around russh: connect, verify the host key, authenticate, run commands.

use crate::model::{AuthMethod, ServerConfig};
use anyhow::{Result, anyhow, bail};
use russh::keys::{PrivateKeyWithHashAlg, check_known_hosts, check_known_hosts_path, load_secret_key};
use russh::{ChannelMsg, Disconnect, client};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub enum ConnectError {
    /// A password or passphrase is required; the string tells the user which.
    NeedsSecret(String),
    Failed(String),
}

impl From<anyhow::Error> for ConnectError {
    fn from(e: anyhow::Error) -> Self {
        ConnectError::Failed(format!("{e:#}"))
    }
}

impl From<russh::Error> for ConnectError {
    fn from(e: russh::Error) -> Self {
        ConnectError::Failed(e.to_string())
    }
}

/// Verifies host keys against `~/.ssh/known_hosts` and gridwatch's own
/// known_hosts file. Unknown hosts are trusted on first use and recorded;
/// a changed key is always rejected.
struct Handler {
    host: String,
    port: u16,
    known_hosts: PathBuf,
}

impl client::Handler for Handler {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        server_key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let russh::keys::PublicKeyOrCertificate::PublicKey { key, .. } = server_key else {
            bail!("host certificates are not supported");
        };
        let changed = |file: &str| {
            anyhow!(
                "HOST KEY CHANGED for {}. Refusing to connect. If this is expected, remove the old entry from {file}.",
                self.host
            )
        };
        match check_known_hosts(&self.host, self.port, key) {
            Ok(true) => return Ok(true),
            Err(russh::keys::Error::KeyChanged { .. }) => return Err(changed("~/.ssh/known_hosts")),
            _ => {}
        }
        match check_known_hosts_path(&self.host, self.port, key, &self.known_hosts) {
            Ok(true) => Ok(true),
            Err(russh::keys::Error::KeyChanged { .. }) => {
                Err(changed(&self.known_hosts.display().to_string()))
            }
            _ => {
                russh::keys::known_hosts::learn_known_hosts_path(&self.host, self.port, key, &self.known_hosts)?;
                Ok(true)
            }
        }
    }
}

pub struct Session {
    handle: client::Handle<Handler>,
}

fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        if let Some(dirs) = directories::UserDirs::new() {
            return dirs.home_dir().join(rest);
        }
    }
    PathBuf::from(path)
}

fn default_keys() -> Vec<PathBuf> {
    let Some(dirs) = directories::UserDirs::new() else {
        return Vec::new();
    };
    ["id_ed25519", "id_ecdsa", "id_rsa"]
        .iter()
        .map(|name| dirs.home_dir().join(".ssh").join(name))
        .filter(|p| p.exists())
        .collect()
}

impl Session {
    pub async fn connect(
        cfg: &ServerConfig,
        secret: Option<&str>,
        known_hosts: &Path,
    ) -> Result<Self, ConnectError> {
        let config = Arc::new(client::Config {
            keepalive_interval: Some(Duration::from_secs(20)),
            keepalive_max: 3,
            ..Default::default()
        });
        let handler = Handler {
            host: cfg.host.clone(),
            port: cfg.port,
            known_hosts: known_hosts.to_path_buf(),
        };
        let connecting = client::connect(config, (cfg.host.as_str(), cfg.port), handler);
        let handle = tokio::time::timeout(Duration::from_secs(12), connecting)
            .await
            .map_err(|_| anyhow!("connection to {}:{} timed out", cfg.host, cfg.port))??;
        let mut session = Session { handle };

        match &cfg.auth {
            AuthMethod::Password => {
                let Some(password) = secret else {
                    return Err(ConnectError::NeedsSecret(format!("Password for {}", cfg.address())));
                };
                if !session.handle.authenticate_password(&cfg.user, password).await?.success() {
                    return Err(ConnectError::NeedsSecret(format!(
                        "Password rejected. Password for {}",
                        cfg.address()
                    )));
                }
            }
            AuthMethod::KeyFile { path } => {
                let path = expand_home(path);
                match session.try_key(&cfg.user, &path, secret).await {
                    Ok(true) => {}
                    Ok(false) => return Err(anyhow!("the server rejected key {}", path.display()).into()),
                    Err(russh::keys::Error::KeyIsEncrypted) => {
                        return Err(ConnectError::NeedsSecret(format!("Passphrase for {}", path.display())));
                    }
                    Err(e) if secret.is_some() => {
                        return Err(ConnectError::NeedsSecret(format!(
                            "Could not unlock key ({e}). Passphrase for {}",
                            path.display()
                        )));
                    }
                    Err(e) => return Err(anyhow!("cannot load key {}: {e}", path.display()).into()),
                }
            }
            AuthMethod::Auto => {
                let mut tried = Vec::new();
                let mut ok = session.try_agent(&cfg.user, &mut tried).await;
                if !ok {
                    for path in default_keys() {
                        match session.try_key(&cfg.user, &path, secret).await {
                            Ok(true) => {
                                ok = true;
                                break;
                            }
                            Ok(false) => tried.push(format!("{} (rejected)", path.display())),
                            Err(e) => tried.push(format!("{} ({e})", path.display())),
                        }
                    }
                }
                if !ok {
                    let detail = if tried.is_empty() {
                        "no SSH agent and no key in ~/.ssh".to_string()
                    } else {
                        tried.join(", ")
                    };
                    return Err(anyhow!("authentication failed: {detail}").into());
                }
            }
        }
        Ok(session)
    }

    async fn try_key(&mut self, user: &str, path: &Path, passphrase: Option<&str>) -> Result<bool, russh::keys::Error> {
        let key = load_secret_key(path, passphrase)?;
        let hash = self.handle.best_supported_rsa_hash().await.ok().flatten().flatten();
        let key = PrivateKeyWithHashAlg::new(Arc::new(key), hash);
        match self.handle.authenticate_publickey(user, key).await {
            Ok(result) => Ok(result.success()),
            Err(_) => Ok(false),
        }
    }

    /// Try every identity the running SSH agent offers.
    async fn try_agent(&mut self, user: &str, tried: &mut Vec<String>) -> bool {
        use russh::keys::agent::client::AgentClient;
        #[cfg(windows)]
        let agent = AgentClient::connect_named_pipe(r"\\.\pipe\openssh-ssh-agent").await;
        #[cfg(unix)]
        let agent = AgentClient::connect_env().await;
        let Ok(agent) = agent else {
            return false;
        };
        let mut agent = agent.dynamic();
        let Ok(identities) = agent.request_identities().await else {
            return false;
        };
        let rsa_hash = self.handle.best_supported_rsa_hash().await.ok().flatten().flatten();
        for identity in identities {
            let key = identity.public_key().into_owned();
            let hash = if key.algorithm().is_rsa() { rsa_hash } else { None };
            match self.handle.authenticate_publickey_with(user, key, hash, &mut agent).await {
                Ok(result) if result.success() => return true,
                _ => tried.push(format!("agent key {} (rejected)", identity.comment())),
            }
        }
        false
    }

    /// Run a command and return its stdout.
    pub async fn exec(&self, command: &str, timeout: Duration) -> Result<String> {
        let run = async {
            let mut channel = self.handle.channel_open_session().await?;
            channel.exec(true, command).await?;
            let mut out = Vec::new();
            while let Some(msg) = channel.wait().await {
                if let ChannelMsg::Data { data } = msg {
                    out.extend_from_slice(&data);
                }
            }
            anyhow::Ok(String::from_utf8_lossy(&out).into_owned())
        };
        tokio::time::timeout(timeout, run)
            .await
            .map_err(|_| anyhow!("remote command timed out"))?
    }

    pub async fn close(&self) {
        let _ = self.handle.disconnect(Disconnect::ByApplication, "", "en").await;
    }
}
