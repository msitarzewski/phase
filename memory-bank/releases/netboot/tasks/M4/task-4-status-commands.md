# Task 4 — Status Commands

**Agent**: Tooling Agent
**Estimated**: 1 day

## 4.1 Implement provider status

- [ ] Query running provider:
  ```rust
  async fn handle_provider_status() -> Result<()> {
      let client = reqwest::Client::new();

      // Try default port
      let url = "http://localhost:8080/status";

      match client.get(url).send().await {
          Ok(resp) if resp.status().is_success() => {
              let status: StatusResponse = resp.json().await?;
              print_status(&status);
          }
          Ok(resp) => {
              println!("Provider returned error: {}", resp.status());
          }
          Err(_) => {
              println!("No provider running on localhost:8080");
              println!("Use 'plasmd serve' to start a provider");
          }
      }

      Ok(())
  }

  fn print_status(status: &StatusResponse) {
      println!("Provider Status");
      println!("===============");
      println!("Version:    {}", status.provider.version);
      println!("Uptime:     {}s", status.provider.uptime_secs);
      println!("Peer ID:    {}", status.provider.peer_id);
      println!();
      println!("Artifacts:");
      for artifact in &status.artifacts.available {
          println!("  {}/{}/{}: {} bytes",
              artifact.channel, artifact.arch, artifact.name, artifact.size_bytes);
      }
      println!();
      println!("Metrics:");
      println!("  Requests:     {}", status.metrics.requests_total);
      println!("  Bytes served: {}", format_bytes(status.metrics.bytes_served_total));
  }
  ```

**Dependencies**: M4/Task 3
**Output**: Status command

---

## 4.2 Implement provider list

- [ ] List advertised artifacts:
  ```rust
  async fn handle_provider_list() -> Result<()> {
      let client = reqwest::Client::new();
      let url = "http://localhost:8080/status";

      let resp = client.get(url).send().await?;
      let status: StatusResponse = resp.json().await?;

      println!("Advertised Artifacts");
      println!("====================");
      println!("{:<10} {:<10} {:<12} {:<15} {}",
          "Channel", "Arch", "Artifact", "Size", "Hash");
      println!("{}", "-".repeat(70));

      for artifact in &status.artifacts.available {
          println!("{:<10} {:<10} {:<12} {:<15} {}...",
              artifact.channel,
              artifact.arch,
              artifact.name,
              format_bytes(artifact.size_bytes),
              &artifact.hash[7..23]);  // First 16 chars of hash
      }

      Ok(())
  }
  ```

**Dependencies**: Task 4.1
**Output**: List command

---

## 4.3 Implement provider keyid

- [ ] Show signing key:
  ```rust
  async fn handle_provider_keyid() -> Result<()> {
      // Option 1: Query running provider
      let client = reqwest::Client::new();
      if let Ok(resp) = client.get("http://localhost:8080/status").send().await {
          if resp.status().is_success() {
              let status: StatusResponse = resp.json().await?;
              println!("{}", status.provider.peer_id);
              return Ok(());
          }
      }

      // Option 2: Load key from disk
      let key_path = default_key_path();
      if key_path.exists() {
          let key = load_signing_key(&key_path)?;
          let verifying = key.verifying_key();
          println!("ed25519:{}", hex::encode(verifying.as_bytes()));
          return Ok(());
      }

      println!("No provider running and no key found");
      Ok(())
  }
  ```

**Dependencies**: Task 4.2
**Output**: Keyid command

---

## 4.4 Add JSON output option

- [ ] Machine-readable output:
  ```rust
  #[derive(Args)]
  struct StatusArgs {
      /// Output as JSON
      #[arg(long)]
      json: bool,
  }

  async fn handle_provider_status(args: StatusArgs) -> Result<()> {
      // ... fetch status ...

      if args.json {
          println!("{}", serde_json::to_string_pretty(&status)?);
      } else {
          print_status(&status);
      }

      Ok(())
  }
  ```

**Dependencies**: Task 4.3
**Output**: JSON output

---

## Validation Checklist

- [ ] `plasmd provider status` shows provider info
- [ ] `plasmd provider list` shows artifacts
- [ ] `plasmd provider keyid` shows signing key
- [ ] Commands work when provider running
- [ ] Clear message when provider not running
- [ ] JSON output option works
