# Task 2 — Configuration File

**Agent**: Tooling Agent
**Estimated**: 2 days

## 2.1 Define config schema

- [ ] Create config structure:
  ```rust
  // daemon/src/provider/config.rs
  use serde::{Deserialize, Serialize};

  #[derive(Debug, Clone, Serialize, Deserialize)]
  pub struct ProviderConfig {
      #[serde(default)]
      pub provider: ProviderSettings,

      #[serde(default)]
      pub dht: DhtSettings,

      #[serde(default)]
      pub mdns: MdnsSettings,
  }

  #[derive(Debug, Clone, Serialize, Deserialize)]
  pub struct ProviderSettings {
      #[serde(default = "default_port")]
      pub port: u16,

      #[serde(default = "default_artifacts_dir")]
      pub artifacts_dir: PathBuf,

      #[serde(default)]
      pub channels: Vec<String>,

      #[serde(default)]
      pub architectures: Vec<String>,

      pub external_ip: Option<String>,

      pub name: Option<String>,
  }

  #[derive(Debug, Clone, Serialize, Deserialize)]
  pub struct DhtSettings {
      #[serde(default = "default_true")]
      pub enabled: bool,

      #[serde(default = "default_refresh")]
      pub refresh_interval_secs: u64,

      pub bootstrap_nodes: Vec<String>,
  }

  #[derive(Debug, Clone, Serialize, Deserialize)]
  pub struct MdnsSettings {
      #[serde(default = "default_true")]
      pub enabled: bool,
  }

  fn default_port() -> u16 { 8080 }
  fn default_artifacts_dir() -> PathBuf { PathBuf::from("/var/lib/plasm/boot-artifacts") }
  fn default_true() -> bool { true }
  fn default_refresh() -> u64 { 1800 }
  ```

**Dependencies**: M4/Task 1
**Output**: Config structure

---

## 2.2 Create example config file

- [ ] Create `examples/provider.toml`:
  ```toml
  # Phase Boot Provider Configuration

  [provider]
  port = 8080
  artifacts_dir = "/var/lib/plasm/boot-artifacts"
  channels = ["stable", "testing"]
  architectures = ["arm64", "x86_64"]
  name = "my-provider"
  # external_ip = "192.168.1.100"  # Optional, auto-detect

  [dht]
  enabled = true
  refresh_interval_secs = 1800
  bootstrap_nodes = [
      "/ip4/104.131.131.82/tcp/4001/p2p/QmaCpDMGvV2BGHeYERUEnRQAwe3N8SzbUtfsmvsqQLuvuJ",
  ]

  [mdns]
  enabled = true
  ```

**Dependencies**: Task 2.1
**Output**: Example config

---

## 2.3 Implement config loading

- [ ] Add config loader:
  ```rust
  impl ProviderConfig {
      pub fn load(path: &Path) -> Result<Self, ConfigError> {
          let content = std::fs::read_to_string(path)
              .map_err(|e| ConfigError::ReadError(e.to_string()))?;

          toml::from_str(&content)
              .map_err(|e| ConfigError::ParseError(e.to_string()))
      }

      pub fn load_or_default(path: Option<&Path>) -> Self {
          match path {
              Some(p) => Self::load(p).unwrap_or_else(|e| {
                  tracing::warn!("Failed to load config: {}, using defaults", e);
                  Self::default()
              }),
              None => Self::default(),
          }
      }
  }
  ```

**Dependencies**: Task 2.2
**Output**: Config loading

---

## 2.4 Merge CLI args with config

- [ ] CLI args override config:
  ```rust
  impl ProviderConfig {
      pub fn merge_cli_args(&mut self, args: &ServeArgs) {
          // CLI args take precedence
          self.provider.port = args.port;

          if args.artifacts != PathBuf::from("/var/lib/plasm/boot-artifacts") {
              self.provider.artifacts_dir = args.artifacts.clone();
          }

          if !args.channel.is_empty() {
              self.provider.channels = vec![args.channel.clone()];
          }

          if let Some(arch) = &args.arch {
              self.provider.architectures = vec![arch.clone()];
          }

          if args.no_dht {
              self.dht.enabled = false;
          }

          if args.no_mdns {
              self.mdns.enabled = false;
          }

          if let Some(ip) = &args.external_ip {
              self.provider.external_ip = Some(ip.clone());
          }
      }
  }
  ```

**Dependencies**: Task 2.3
**Output**: CLI-config merge

---

## 2.5 Platform-specific defaults

- [ ] Different defaults per platform:
  ```rust
  impl Default for ProviderSettings {
      fn default() -> Self {
          Self {
              port: 8080,
              artifacts_dir: platform_artifacts_dir(),
              channels: vec!["stable".to_string()],
              architectures: vec![current_arch()],
              external_ip: None,
              name: None,
          }
      }
  }

  fn platform_artifacts_dir() -> PathBuf {
      #[cfg(target_os = "macos")]
      {
          dirs::data_dir()
              .map(|d| d.join("plasm/boot-artifacts"))
              .unwrap_or_else(|| PathBuf::from("/var/lib/plasm/boot-artifacts"))
      }

      #[cfg(target_os = "linux")]
      {
          PathBuf::from("/var/lib/plasm/boot-artifacts")
      }
  }

  fn current_arch() -> String {
      #[cfg(target_arch = "aarch64")]
      { "arm64".to_string() }

      #[cfg(target_arch = "x86_64")]
      { "x86_64".to_string() }
  }
  ```

**Dependencies**: Task 2.4
**Output**: Platform defaults

---

## Validation Checklist

- [ ] Config file parses correctly
- [ ] Missing fields use defaults
- [ ] CLI args override config
- [ ] Platform-specific paths work
- [ ] Example config documented
