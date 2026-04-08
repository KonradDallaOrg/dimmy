//! About tab — app info, version, links.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use dimmy_lib::AppState;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;

pub fn create_page(
    _app_state: &Arc<AppState>,
    _show_advanced: &Rc<Cell<bool>>,
) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("About")
        .icon_name("help-about-symbolic")
        .build();

    // ── App identity group ───────────────────────────────────────────
    let identity_group = adw::PreferencesGroup::new();

    // App icon placeholder
    let icon = gtk4::Image::builder()
        .icon_name("audio-input-microphone-symbolic")
        .pixel_size(64)
        .margin_top(24)
        .margin_bottom(8)
        .halign(gtk4::Align::Center)
        .build();

    let title_label = gtk4::Label::builder()
        .label("Dimmy")
        .css_classes(vec!["title-1".to_string()])
        .halign(gtk4::Align::Center)
        .build();

    let version_label = gtk4::Label::builder()
        .label(format!("Version {}", env!("CARGO_PKG_VERSION")))
        .css_classes(vec!["dim-label".to_string()])
        .halign(gtk4::Align::Center)
        .build();

    let tagline_label = gtk4::Label::builder()
        .label("Voice dictation that stays out of your way")
        .css_classes(vec!["dim-label".to_string()])
        .halign(gtk4::Align::Center)
        .margin_bottom(16)
        .build();

    let header_box = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(4)
        .halign(gtk4::Align::Center)
        .build();
    header_box.append(&icon);
    header_box.append(&title_label);
    header_box.append(&version_label);
    header_box.append(&tagline_label);

    // Use a gtk4::ListBoxRow to embed the header box in the group
    let header_row = gtk4::ListBoxRow::builder()
        .selectable(false)
        .activatable(false)
        .child(&header_box)
        .build();
    identity_group.add(&header_row);

    page.add(&identity_group);

    // ── Actions group ────────────────────────────────────────────────
    let actions_group = adw::PreferencesGroup::builder().title("Updates").build();

    let update_btn = gtk4::Button::builder()
        .label("Check for Updates")
        .css_classes(vec!["flat".to_string()])
        .build();

    let update_row = adw::ActionRow::builder()
        .title("Check for Updates")
        .subtitle("Look for a newer version of Dimmy")
        .activatable_widget(&update_btn)
        .build();

    {
        let update_row_clone = update_row.clone();
        let update_btn_clone = update_btn.clone();
        update_btn.connect_clicked(move |_| {
            update_btn_clone.set_sensitive(false);
            update_row_clone.set_subtitle("Checking...");

            let row = update_row_clone.clone();
            let btn = update_btn_clone.clone();

            // Use async_channel to avoid moving non-Send GTK widgets into std::thread
            let (sender, receiver) = async_channel::bounded(1);
            std::thread::spawn(move || {
                let result = check_github_release();
                let _ = sender.send_blocking(result);
            });

            glib::spawn_future_local(async move {
                if let Ok(result) = receiver.recv().await {
                    match result {
                        Ok(Some((version, url))) => {
                            let subtitle = format!(
                                "Update available: v{} — <a href=\"{}\">Download</a>",
                                version, url
                            );
                            row.set_subtitle(&subtitle);
                            row.set_use_markup(true);
                        }
                        Ok(None) => {
                            row.set_subtitle("You're on the latest version");
                        }
                        Err(e) => {
                            // Truncate error to 100 chars to prevent PII/key leak
                            let truncated: String = e.chars().take(100).collect();
                            row.set_subtitle(&format!("Update check failed: {truncated}"));
                        }
                    }
                    btn.set_sensitive(true);
                }
            });
        });
    }

    update_row.add_suffix(&update_btn);
    actions_group.add(&update_row);

    page.add(&actions_group);

    // ── Links group ──────────────────────────────────────────────────
    let links_group = adw::PreferencesGroup::builder().title("Links").build();

    let github_row = adw::ActionRow::builder()
        .title("GitHub Repository")
        .subtitle("https://github.com/KonradDallaOrg/dimmy")
        .build();
    let github_btn = gtk4::Button::builder()
        .label("Open")
        .css_classes(vec!["flat".to_string()])
        .valign(gtk4::Align::Center)
        .build();
    github_btn.connect_clicked(move |_| {
        let _ = std::process::Command::new("xdg-open")
            .arg("https://github.com/KonradDallaOrg/dimmy")
            .spawn();
    });
    github_row.add_suffix(&github_btn);
    links_group.add(&github_row);

    let issues_row = adw::ActionRow::builder()
        .title("Report a Bug")
        .subtitle("https://github.com/KonradDallaOrg/dimmy/issues")
        .build();
    let issues_btn = gtk4::Button::builder()
        .label("Open")
        .css_classes(vec!["flat".to_string()])
        .valign(gtk4::Align::Center)
        .build();
    issues_btn.connect_clicked(move |_| {
        let _ = std::process::Command::new("xdg-open")
            .arg("https://github.com/KonradDallaOrg/dimmy/issues")
            .spawn();
    });
    issues_row.add_suffix(&issues_btn);
    links_group.add(&issues_row);

    page.add(&links_group);

    // ── Footer ───────────────────────────────────────────────────────
    let footer_group = adw::PreferencesGroup::new();
    let footer_label = gtk4::Label::builder()
        .label("Made with irony")
        .css_classes(vec!["dim-label".to_string()])
        .halign(gtk4::Align::Center)
        .margin_top(16)
        .margin_bottom(16)
        .build();
    let footer_row = gtk4::ListBoxRow::builder()
        .selectable(false)
        .activatable(false)
        .child(&footer_label)
        .build();
    footer_group.add(&footer_row);

    page.add(&footer_group);

    page
}

/// Query GitHub API for the latest release.
///
/// Returns `Ok(Some((version, url)))` if a newer version is available,
/// `Ok(None)` if already on the latest version, or `Err(msg)` on failure.
fn check_github_release() -> Result<Option<(String, String)>, String> {
    const API_URL: &str = "https://api.github.com/repos/KonradDallaOrg/dimmy/releases/latest";

    // Validate URL — must be HTTPS (not localhost)
    assert!(
        API_URL.starts_with("https://"),
        "GitHub API URL must use HTTPS"
    );

    let output = std::process::Command::new("curl")
        .args([
            "--silent",
            "--fail-with-body",
            "--max-time",
            "15",
            "--user-agent",
            "dimmy-linux-updater",
            "-H",
            "Accept: application/vnd.github+json",
            API_URL,
        ])
        .output()
        .map_err(|e| {
            let msg = e.to_string();
            // Truncate to 100 chars
            msg.chars().take(100).collect::<String>()
        })?;

    if !output.status.success() {
        let code = output.status.code().unwrap_or(-1);
        return Err(format!("curl exited with status {code}"));
    }

    let body = std::str::from_utf8(&output.stdout)
        .map_err(|_| "Response is not valid UTF-8".to_string())?;

    if body.is_empty() {
        return Err("Empty response from GitHub API".to_string());
    }

    let json: serde_json::Value = serde_json::from_str(body).map_err(|e| {
        let msg = e.to_string();
        msg.chars().take(100).collect::<String>()
    })?;

    let tag = json["tag_name"]
        .as_str()
        .ok_or_else(|| "Missing tag_name in GitHub response".to_string())?;

    let html_url = json["html_url"]
        .as_str()
        .ok_or_else(|| "Missing html_url in GitHub response".to_string())?;

    assert!(!tag.is_empty(), "tag_name must not be empty");
    assert!(!html_url.is_empty(), "html_url must not be empty");

    // Strip leading 'v' if present (e.g. "v0.3.64" → "0.3.64")
    let remote_version = tag.trim_start_matches('v');

    assert!(
        !remote_version.is_empty(),
        "stripped remote version must not be empty"
    );

    let local_version = env!("CARGO_PKG_VERSION");

    if is_newer(remote_version, local_version) {
        Ok(Some((remote_version.to_string(), html_url.to_string())))
    } else {
        Ok(None)
    }
}

/// Return `true` if `remote` is a strictly newer semver than `local`.
///
/// Parses `MAJOR.MINOR.PATCH`; any component that fails to parse is treated as 0.
fn is_newer(remote: &str, local: &str) -> bool {
    assert!(!remote.is_empty(), "remote version must not be empty");
    assert!(!local.is_empty(), "local version must not be empty");

    fn parse(v: &str) -> (u32, u32, u32) {
        let mut parts = v.splitn(3, '.');
        let major = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let minor = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let patch = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        (major, minor, patch)
    }

    parse(remote) > parse(local)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_newer_patch() {
        assert!(is_newer("0.3.65", "0.3.64"));
    }

    #[test]
    fn test_is_newer_minor() {
        assert!(is_newer("0.4.0", "0.3.64"));
    }

    #[test]
    fn test_is_newer_major() {
        assert!(is_newer("1.0.0", "0.3.64"));
    }

    #[test]
    fn test_not_newer_same() {
        assert!(!is_newer("0.3.64", "0.3.64"));
    }

    #[test]
    fn test_not_newer_older() {
        assert!(!is_newer("0.3.63", "0.3.64"));
    }

    #[test]
    fn test_not_newer_older_minor() {
        assert!(!is_newer("0.2.99", "0.3.0"));
    }

    #[test]
    fn test_v_prefix_stripped_by_caller() {
        // is_newer receives already-stripped versions from check_github_release
        assert!(is_newer("1.0.0", "0.9.9"));
    }
}
