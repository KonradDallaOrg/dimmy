//! License CLI — interactive client for the licensing-server PoC.
//!
//! Subcommands:
//!   request-trial <email>       provision a 14-day trial license
//!   simulate-purchase <email> <tier>  fake the Lemon Squeezy webhook
//!   activate <code-or-link>     redeem the activation code → save token
//!   refresh                     bump last_seen + re-issue token
//!   status                      verify the on-disk token, print state
//!   info                        show file paths + embedded pubkey hint
//!
//! Defaults talk to http://localhost:8787 — override with
//! `--server <url>` per command. Saves the token to the standard
//! Dimmy config location (`~/.config/dimmy/license.json`).

#[cfg(feature = "license-cli")]
fn main() {
    if let Err(e) = cli::run() {
        eprintln!("error: {}", e);
        std::process::exit(1);
    }
}

#[cfg(not(feature = "license-cli"))]
fn main() {
    eprintln!(
        "license_cli binary requires the `license-cli` feature.\n\
         Build with: cargo build --bin license_cli --features license-cli"
    );
    std::process::exit(1);
}

#[cfg(feature = "license-cli")]
mod cli {
    use clap::{Parser, Subcommand, ValueEnum};
    use dimmy_lib::license::{
        check_status, last_online_check, last_online_check_path, license_path, load_license_file,
        redeem_activation_code, refresh_token, request_trial, save_license_file,
        set_last_online_check, EMBEDDED_PUBKEY_B64,
    };
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[derive(Parser)]
    #[command(name = "license_cli", about = "Dimmy licensing PoC client")]
    struct Cli {
        #[arg(long, default_value = "http://localhost:8787")]
        server: String,
        #[command(subcommand)]
        command: Command,
    }

    #[derive(Subcommand)]
    enum Command {
        /// Provision a 14-day trial license for the given email.
        RequestTrial { email: String },
        /// Simulate a paid purchase (Lemon Squeezy webhook stand-in).
        SimulatePurchase {
            email: String,
            #[arg(value_enum)]
            tier: TierArg,
        },
        /// Redeem an activation code (or full magic link) — saves the
        /// signed token to the license file on disk.
        Activate {
            /// Either the raw `code` or the full `http://…/api/activate?code=…` URL.
            code_or_link: String,
            #[arg(long, default_value = "CLI dev device")]
            device_label: String,
        },
        /// Re-issue the on-disk token + bump server-side `last_seen`.
        Refresh,
        /// Verify on-disk token, print parsed claims + computed status.
        Status,
        /// Show paths + embedded pubkey hint.
        Info,
    }

    #[derive(Copy, Clone, ValueEnum)]
    enum TierArg {
        Monthly,
        Annual,
        Lifetime,
    }

    impl TierArg {
        fn as_wire(self) -> &'static str {
            match self {
                TierArg::Monthly => "monthly",
                TierArg::Annual => "annual",
                TierArg::Lifetime => "lifetime",
            }
        }
    }

    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        let cli = Cli::parse();
        let rt = tokio::runtime::Runtime::new()?;
        match cli.command {
            Command::RequestTrial { email } => rt.block_on(request_trial_cmd(&cli.server, &email)),
            Command::SimulatePurchase { email, tier } => {
                rt.block_on(simulate_purchase_cmd(&cli.server, &email, tier))
            }
            Command::Activate {
                code_or_link,
                device_label,
            } => rt.block_on(activate_cmd(&cli.server, &code_or_link, &device_label)),
            Command::Refresh => rt.block_on(refresh_cmd(&cli.server)),
            Command::Status => status_cmd(),
            Command::Info => info_cmd(),
        }
    }

    async fn request_trial_cmd(
        server: &str,
        email: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let resp = request_trial(server, email).await?;
        println!("magic_link: {}", resp.magic_link);
        println!();
        println!("(in production this URL would be in the email; for the PoC, run:");
        println!("  license_cli activate \"{}\"", resp.magic_link);
        println!(")");
        Ok(())
    }

    async fn simulate_purchase_cmd(
        server: &str,
        email: &str,
        tier: TierArg,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let url = format!("{}/api/license/issue", server.trim_end_matches('/'));
        let body = serde_json::json!({ "email": email, "tier": tier.as_wire() });
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()?;
        let resp = client.post(&url).json(&body).send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(format!("HTTP {}: {}", status, text).into());
        }
        let parsed: serde_json::Value = serde_json::from_str(&text)?;
        let magic = parsed
            .get("magic_link")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        println!("magic_link: {}", magic);
        Ok(())
    }

    async fn activate_cmd(
        server: &str,
        code_or_link: &str,
        device_label: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let code = extract_code(code_or_link)?;
        let resp = redeem_activation_code(server, &code, device_label).await?;
        save_license_file(&resp.token)?;
        // Stamp last_online_check so the offline-grace clock starts now.
        set_last_online_check(now_secs())?;
        let path = license_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(no config dir)".into());
        println!("activated. token saved to {}", path);
        println!();
        println!("--- token ({} bytes) ---", resp.token.len());
        println!("{}", resp.token);
        Ok(())
    }

    async fn refresh_cmd(server: &str) -> Result<(), Box<dyn std::error::Error>> {
        let token = match load_license_file()? {
            Some(t) => t,
            None => return Err("no license on disk; run `activate` first".into()),
        };
        let resp = refresh_token(server, &token).await?;
        save_license_file(&resp.token)?;
        set_last_online_check(now_secs())?;
        println!("refreshed. token rotated.");
        Ok(())
    }

    fn status_cmd() -> Result<(), Box<dyn std::error::Error>> {
        let status = check_status();
        println!("status: {:?}", status);
        if let Ok(Some(token)) = load_license_file() {
            println!("token (raw, on-disk): {} bytes", token.len());
            // Decode payload segment for human readability — best-effort,
            // doesn't fail the command if it can't.
            if let Some(payload_b64) = token.split('.').nth(1) {
                use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
                if let Ok(bytes) = URL_SAFE_NO_PAD.decode(payload_b64) {
                    if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                        println!("claims: {}", serde_json::to_string_pretty(&json)?);
                    }
                }
            }
        } else {
            println!("(no license file on disk)");
        }
        let last_online = last_online_check();
        if last_online > 0 {
            let now = now_secs();
            let days = (now - last_online) / 86_400;
            println!("last online check: {} ({} days ago)", last_online, days);
        } else {
            println!("last online check: (never)");
        }
        Ok(())
    }

    fn info_cmd() -> Result<(), Box<dyn std::error::Error>> {
        println!("--- License CLI info ---");
        let lpath: PathBuf = license_path().unwrap_or_default();
        let opath: PathBuf = last_online_check_path().unwrap_or_default();
        println!("license file:        {}", lpath.display());
        println!("last_online sidecar: {}", opath.display());
        if EMBEDDED_PUBKEY_B64.is_empty() {
            println!("embedded pubkey:     (none — source build, licensing bypassed)");
        } else {
            println!(
                "embedded pubkey:     {} (len {})",
                preview(EMBEDDED_PUBKEY_B64, 24),
                EMBEDDED_PUBKEY_B64.len()
            );
        }
        Ok(())
    }

    fn preview(s: &str, n: usize) -> String {
        if s.len() <= n {
            s.to_string()
        } else {
            format!("{}…", &s[..n])
        }
    }

    /// Accept either a bare activation code or a full magic-link URL
    /// and return just the code.
    fn extract_code(input: &str) -> Result<String, Box<dyn std::error::Error>> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err("empty input".into());
        }
        if let Some(qmark) = trimmed.find('?') {
            // URL-ish — parse the query string.
            let query = &trimmed[qmark + 1..];
            for pair in query.split('&') {
                let mut iter = pair.splitn(2, '=');
                let k = iter.next().unwrap_or("");
                let v = iter.next().unwrap_or("");
                if k == "code" {
                    return Ok(v.to_string());
                }
            }
            Err("magic link missing `code` parameter".into())
        } else {
            Ok(trimmed.to_string())
        }
    }

    fn now_secs() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }
}
