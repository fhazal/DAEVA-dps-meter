//! Updating the meter itself (tauri-plugin-updater), only when it starts.
//! A version that comes out while the meter runs shows the usual
//! "available · Download" line; the next start installs it.
//!
//! Each attempt is written to the settings before the installer runs (the
//! meter quits for it), so the next start can tell "updated" from "the
//! installer never finished" (antivirus). One retry, then the meter stops
//! trying that version and shows the normal Download line.

use crate::state::{self, Shared};
use meter_core::json::{Obj, Value};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Manager};
use tauri_plugin_updater::UpdaterExt;

/// What the green (or amber) update bar shows.
#[derive(Clone, Debug)]
pub struct SelfUpdate {
    /// "downloading", "installing", "updated", "blocked" or "failed".
    pub stage: &'static str,
    pub version: String,
    /// The version before an update ("updated" only).
    pub from: String,
    pub pct: Option<u8>,
    pub reason: Option<String>,
    /// When the stage began (unix time); "updated" fades after 10 s.
    pub at: f64,
}

impl SelfUpdate {
    fn new(stage: &'static str, version: &str) -> Self {
        SelfUpdate { stage, version: version.to_string(), from: String::new(), pct: None, reason: None, at: state::now_unix() }
    }

    /// For the status feed; None once "updated" has faded.
    pub fn view(&self) -> Option<Value> {
        if self.stage == "updated" && state::now_unix() - self.at > 10.0 {
            return None;
        }
        Some(
            Obj::new()
                .set("stage", self.stage)
                .set("version", self.version.as_str())
                .set("from", self.from.as_str())
                .set("pct", self.pct)
                .set("reason", self.reason.clone())
                .set("url", format!("{}/tag/v{}", state::RELEASES_URL, self.version))
                .build(),
        )
    }
}

const BLOCKED: &str = "The installer didn't finish. Antivirus may have stopped it.";

pub fn start(app: AppHandle, shared: Arc<Shared>) {
    // The installer's version (tauri.conf), the one the updater compares:
    // test builds set it without touching Cargo.toml.
    let cur = app.package_info().version.to_string();
    tauri::async_runtime::spawn(async move {
        if !after_last_attempt(&shared, &cur) {
            return;
        }
        if let Err(e) = run(&app, &shared, &cur).await {
            let mut c = shared.lock();
            if let Some(u) = c.self_update.as_mut() {
                u.stage = "failed";
                u.reason = Some(e);
                u.at = state::now_unix();
            }
        }
    });
}

/// Sorts out the attempt from the last run. False: don't try again now.
fn after_last_attempt(shared: &Shared, cur: &str) -> bool {
    let mut c = shared.lock();
    let to = c.settings.updating_to.clone();
    if to.is_empty() {
        return true;
    }
    // Any other version than before the attempt: the installer ran.
    if c.settings.updated_from != cur {
        let mut u = SelfUpdate::new("updated", cur);
        u.from = std::mem::take(&mut c.settings.updated_from);
        c.self_update = Some(u);
        c.settings.updating_to.clear();
        c.settings.update_tries = 0;
        c.save_settings();
        return false;
    }
    if c.settings.update_tries >= 2 {
        // Tried twice: leave it to the user (and the Download line).
        let mut u = SelfUpdate::new("blocked", &to);
        u.reason = Some(BLOCKED.into());
        c.self_update = Some(u);
        c.settings.auto_update_gave_up = to;
        c.settings.updating_to.clear();
        c.settings.update_tries = 0;
        c.save_settings();
        return false;
    }
    true
}

async fn run(app: &AppHandle, shared: &Arc<Shared>, cur: &str) -> Result<(), String> {
    let (auto, test) = {
        let c = shared.lock();
        (c.settings.auto_update && c.settings.check_updates, c.settings.test_updates)
    };
    if !auto {
        return Ok(());
    }
    let mut builder = app.updater_builder();
    if test {
        // Pre-releases aren't "latest" on GitHub: find the newest one.
        let url = tauri::async_runtime::spawn_blocking(newest_update_file).await.map_err(|e| e.to_string())??;
        builder = builder.endpoints(vec![url.parse::<tauri::Url>().map_err(|e| e.to_string())?]).map_err(|e| e.to_string())?;
    }
    let Some(update) = builder.build().map_err(|e| e.to_string())?.check().await.map_err(|e| e.to_string())? else {
        return Ok(());
    };
    let version = update.version.clone();
    {
        let mut c = shared.lock();
        if c.settings.auto_update_gave_up == version {
            return Ok(());
        }
        c.self_update = Some(SelfUpdate::new("downloading", &version));
    }
    let mut got = 0u64;
    let sh = shared.clone();
    let bytes = update
        .download(
            move |chunk, total| {
                got += chunk as u64;
                if let (Some(t), Some(u)) = (total.filter(|t| *t > 0), sh.lock().self_update.as_mut()) {
                    u.pct = Some((got * 100 / t).min(100) as u8);
                }
            },
            || {},
        )
        .await
        .map_err(|e| format!("Download failed: {e}"))?;
    // Not in the middle of a fight (it's the start, but just in case).
    while shared.lock().pipeline.engine.current.as_ref().map_or(false, |f| !f.finished) {
        tokio_sleep(Duration::from_secs(2)).await;
    }
    {
        let mut c = shared.lock();
        if let Some(u) = c.self_update.as_mut() {
            u.stage = "installing";
            u.pct = None;
        }
        c.settings.updating_to = version.clone();
        c.settings.updated_from = cur.to_string();
        c.settings.update_tries += 1;
        c.save_settings();
    }
    // Let the bar show "Installing" for a moment; the installer closes us.
    tokio_sleep(Duration::from_millis(1500)).await;
    if let Err(e) = update.install(bytes) {
        let mut c = shared.lock();
        let mut u = SelfUpdate::new("blocked", &version);
        u.reason = Some(format!("{BLOCKED} ({e})"));
        c.self_update = Some(u);
        c.settings.updating_to.clear();
        c.save_settings();
    }
    Ok(())
}

async fn tokio_sleep(d: Duration) {
    let _ = tauri::async_runtime::spawn_blocking(move || std::thread::sleep(d)).await;
}

/// The update file of the newest published release, pre-releases included.
fn newest_update_file() -> Result<String, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(20))
        .user_agent(concat!("DaevaMeter/", env!("CARGO_PKG_VERSION"), " (+https://github.com/fhazal/DAEVA-dps-meter)"))
        .build();
    let list: serde_json::Value = agent
        .get(state::RELEASES_API)
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| e.to_string())?
        .into_string()
        .map_err(|e| e.to_string())
        .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))?;
    let newest = list
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| !r["draft"].as_bool().unwrap_or(false))
        .max_by(|a, b| state::version_key(a["tag_name"].as_str().unwrap_or("")).cmp(&state::version_key(b["tag_name"].as_str().unwrap_or(""))))
        .ok_or("no releases")?;
    newest["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["name"] == "latest.json")
        .and_then(|a| a["browser_download_url"].as_str())
        .filter(|u| u.starts_with(state::RELEASES_URL))
        .map(String::from)
        .ok_or_else(|| "the newest release has no update file".into())
}
