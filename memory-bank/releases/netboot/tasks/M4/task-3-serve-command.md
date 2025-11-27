# Task 3 — Serve Command Implementation

**Agent**: Tooling Agent
**Estimated**: 2 days

## 3.1 Implement serve command handler

- [ ] Add to main.rs:
  ```rust
  async fn handle_serve(args: ServeArgs) -> Result<()> {
      // Load config
      let mut config = ProviderConfig::load_or_default(args.config.as_deref());
      config.merge_cli_args(&args);

      // Validate artifacts directory
      if !config.provider.artifacts_dir.exists() {
          return Err(anyhow!("Artifacts directory does not exist: {:?}",
              config.provider.artifacts_dir));
      }

      // Load or generate signing key
      let signing_key = load_or_generate_key(&config)?;

      // Create provider state
      let state = Arc::new(RwLock::new(ProviderState::new(
          config.provider.clone(),
          signing_key.clone(),
      )));

      // Start HTTP server
      let server = ProviderServer::new(state.clone(), config.provider.port);
      let server_handle = tokio::spawn(server.run());

      // Start discovery (DHT)
      let mut discovery = if config.dht.enabled {
          Some(Discovery::new(DiscoveryConfig::from(&config))?)
      } else {
          None
      };

      // Start mDNS
      let mut mdns = if config.mdns.enabled {
          Some(MdnsAdvertiser::new()?)
      } else {
          None
      };

      // Advertise artifacts
      if let Some(ref mut disc) = discovery {
          // ... advertise to DHT
      }
      if let Some(ref mut m) = mdns {
          // ... advertise via mDNS
      }

      tracing::info!("Provider started on port {}", config.provider.port);

      // Wait for shutdown signal
      tokio::signal::ctrl_c().await?;

      tracing::info!("Shutting down...");
      Ok(())
  }
  ```

**Dependencies**: M4/Task 2
**Output**: Serve command handler

---

## 3.2 Add startup validation

- [ ] Validate before starting:
  ```rust
  fn validate_provider_setup(config: &ProviderConfig) -> Result<()> {
      // Check artifacts exist
      let available = scan_available_artifacts(&config.provider.artifacts_dir);
      if available.is_empty() {
          return Err(anyhow!("No artifacts found in {:?}",
              config.provider.artifacts_dir));
      }

      // Check port available
      if !is_port_available(config.provider.port) {
          return Err(anyhow!("Port {} is already in use",
              config.provider.port));
      }

      // Log what will be advertised
      tracing::info!("Found {} artifact combinations:", available.len());
      for (channel, arch) in &available {
          tracing::info!("  - {}/{}", channel, arch);
      }

      Ok(())
  }
  ```

**Dependencies**: Task 3.1
**Output**: Startup validation

---

## 3.3 Add graceful shutdown

- [ ] Handle signals:
  ```rust
  // In handle_serve
  let shutdown = async {
      let ctrl_c = tokio::signal::ctrl_c();

      #[cfg(unix)]
      let terminate = async {
          tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
              .expect("signal handler")
              .recv()
              .await
      };

      #[cfg(not(unix))]
      let terminate = std::future::pending::<()>();

      tokio::select! {
          _ = ctrl_c => {},
          _ = terminate => {},
      }
  };

  tokio::select! {
      result = server_handle => {
          result??;
      }
      _ = shutdown => {
          tracing::info!("Received shutdown signal");
      }
  }

  // Cleanup
  if let Some(mut mdns) = mdns {
      mdns.stop()?;
  }

  tracing::info!("Provider stopped");
  ```

**Dependencies**: Task 3.2
**Output**: Graceful shutdown

---

## 3.4 Add status output on startup

- [ ] Print useful info:
  ```rust
  fn print_startup_info(config: &ProviderConfig, peer_id: &str) {
      println!("╔══════════════════════════════════════════════╗");
      println!("║           Phase Boot Provider                ║");
      println!("╠══════════════════════════════════════════════╣");
      println!("║ HTTP:     http://0.0.0.0:{}              ║", config.provider.port);
      println!("║ Peer ID:  {}...║", &peer_id[..20]);
      println!("║ DHT:      {}                              ║",
          if config.dht.enabled { "enabled" } else { "disabled" });
      println!("║ mDNS:     {}                              ║",
          if config.mdns.enabled { "enabled" } else { "disabled" });
      println!("╠══════════════════════════════════════════════╣");
      println!("║ Press Ctrl+C to stop                         ║");
      println!("╚══════════════════════════════════════════════╝");
  }
  ```

**Dependencies**: Task 3.3
**Output**: Startup display

---

## Validation Checklist

- [ ] `plasmd serve` starts provider
- [ ] Missing artifacts detected
- [ ] Port conflicts detected
- [ ] Ctrl+C stops cleanly
- [ ] Startup info displayed
- [ ] Config merged correctly
