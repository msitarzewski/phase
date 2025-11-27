# Task 1 — CLI Command Structure

**Agent**: Tooling Agent
**Estimated**: 2 days

## 1.1 Extend existing CLI with provider commands

- [ ] Update `daemon/src/main.rs` clap definitions:
  ```rust
  #[derive(Parser)]
  #[command(name = "plasmd")]
  struct Cli {
      #[command(subcommand)]
      command: Commands,
  }

  #[derive(Subcommand)]
  enum Commands {
      /// Run as daemon (existing)
      Daemon(DaemonArgs),

      /// Run a WASM file (existing)
      Run(RunArgs),

      /// Start as boot artifact provider
      Serve(ServeArgs),

      /// Provider operations
      Provider {
          #[command(subcommand)]
          command: ProviderCommands,
      },

      /// Manifest operations
      Manifest {
          #[command(subcommand)]
          command: ManifestCommands,
      },
  }
  ```

**Dependencies**: None
**Output**: CLI structure defined

---

## 1.2 Define serve command arguments

- [ ] Add ServeArgs:
  ```rust
  #[derive(Args)]
  struct ServeArgs {
      /// Path to boot artifacts directory
      #[arg(short, long, default_value = "/var/lib/plasm/boot-artifacts")]
      artifacts: PathBuf,

      /// Channel to advertise
      #[arg(short, long, default_value = "stable")]
      channel: String,

      /// Architecture (auto-detect if not specified)
      #[arg(short = 'A', long)]
      arch: Option<String>,

      /// HTTP port
      #[arg(short, long, default_value = "8080")]
      port: u16,

      /// Config file path
      #[arg(short = 'C', long)]
      config: Option<PathBuf>,

      /// Disable DHT advertisement
      #[arg(long)]
      no_dht: bool,

      /// Disable mDNS advertisement
      #[arg(long)]
      no_mdns: bool,

      /// External IP (for DHT advertisement, auto-detect if not specified)
      #[arg(long)]
      external_ip: Option<String>,
  }
  ```

**Dependencies**: Task 1.1
**Output**: Serve arguments defined

---

## 1.3 Define provider subcommands

- [ ] Add ProviderCommands:
  ```rust
  #[derive(Subcommand)]
  enum ProviderCommands {
      /// Show provider status
      Status,

      /// List advertised artifacts
      List,

      /// Show signing key ID
      Keyid,
  }
  ```

**Dependencies**: Task 1.2
**Output**: Provider subcommands

---

## 1.4 Define manifest subcommands

- [ ] Add ManifestCommands:
  ```rust
  #[derive(Subcommand)]
  enum ManifestCommands {
      /// Generate manifest from artifacts
      Generate {
          #[arg(short, long)]
          artifacts: PathBuf,

          #[arg(short, long)]
          output: PathBuf,

          #[arg(long)]
          sign: bool,

          #[arg(long, default_value = "stable")]
          channel: String,

          #[arg(long)]
          arch: Option<String>,
      },

      /// Show manifest for current artifacts
      Show {
          #[arg(short, long, default_value = "stable")]
          channel: String,

          #[arg(short, long)]
          arch: Option<String>,
      },

      /// Verify manifest signature
      Verify {
          #[arg(short, long)]
          manifest: PathBuf,

          #[arg(short, long)]
          pubkey: Option<PathBuf>,
      },
  }
  ```

**Dependencies**: Task 1.3
**Output**: Manifest subcommands

---

## 1.5 Update help text

- [ ] Add detailed help for each command:
  ```rust
  /// Start as boot artifact provider
  ///
  /// Serves boot artifacts (kernel, initramfs, rootfs) over HTTP
  /// and advertises availability via DHT and mDNS.
  ///
  /// Examples:
  ///   plasmd serve --artifacts /path/to/artifacts
  ///   plasmd serve --channel testing --port 9090
  ///   plasmd serve --no-dht  # LAN only
  #[command(verbatim_doc_comment)]
  Serve(ServeArgs),
  ```

**Dependencies**: Task 1.4
**Output**: Help text complete

---

## Validation Checklist

- [ ] `plasmd --help` shows all commands
- [ ] `plasmd serve --help` shows serve options
- [ ] `plasmd provider --help` shows subcommands
- [ ] `plasmd manifest --help` shows subcommands
- [ ] Arguments parse correctly
- [ ] Default values applied
