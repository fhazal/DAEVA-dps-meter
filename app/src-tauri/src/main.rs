#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod npcap;
mod selfupdate;
mod settings;
mod state;
mod winsys;

use settings::Settings;
use state::Shared;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

type St<'a> = tauri::State<'a, Arc<Shared>>;

// ---------------------------------------------------------------- commands

#[tauri::command]
fn get_snapshot(st: St<'_>, target: Option<u64>) -> String {
    st.lock().snapshot(target).to_string()
}

#[tauri::command]
fn get_detail(st: St<'_>, fight_id: Option<u64>, actor_id: u64, target: Option<u64>) -> Result<String, String> {
    let c = st.lock();
    let e = &c.pipeline.engine;
    let f = match fight_id {
        Some(id) => e.fight(id),
        None => e.display_fight(),
    }
    .ok_or("fight not found")?;
    let d = e.actor_detail_for(f, actor_id, target).ok_or("player not in this fight")?;
    Ok(d.to_string())
}

#[tauri::command]
fn get_history(st: St<'_>) -> String {
    let c = st.lock();
    let mut session = c.pipeline.engine.history_list();
    c.bests.mark(&mut session);
    meter_core::json::Obj::new().set("session", session).set("saved", c.saved_fights()).build().to_string()
}

#[tauri::command]
fn get_fight_record(st: St<'_>, fight_id: Option<u64>, file: Option<String>) -> Result<String, String> {
    let c = st.lock();
    let mut rec = if let Some(file) = file {
        let src = state::read_saved_fight(&c.paths.history, &file, &c.pipeline.engine.names)?;
        meter_core::json::parse(&src).map_err(|e| e.to_string())?
    } else {
        let e = &c.pipeline.engine;
        let f = match fight_id {
            Some(id) => e.fight(id),
            None => e.display_fight(),
        }
        .ok_or("fight not found")?;
        e.fight_record(f)
    };
    // "Your best on this boss" line in History.
    let row = meter_core::engine::history_row(&rec);
    if let (Some((dps, at, count)), meter_core::json::Value::Obj(m)) = (c.bests.best_for(&row), &mut rec) {
        let best = meter_core::json::Obj::new().set("dps", dps).set("started_unix", at).set("this", c.bests.is_best(&row)).set("count", count).build();
        m.insert("best".into(), best);
    }
    Ok(rec.to_string())
}

fn copy_summary_inner(shared: &Shared, fight_id: Option<u64>) -> Result<String, String> {
    let text = {
        let c = shared.lock();
        let e = &c.pipeline.engine;
        let f = match fight_id {
            Some(id) => e.fight(id),
            None => e.display_fight(),
        }
        .ok_or("no fight to copy yet")?;
        e.summary_text(f, 12)
    };
    winsys::set_clipboard(&text)?;
    Ok(text)
}

#[tauri::command]
fn copy_summary(st: St<'_>, fight_id: Option<u64>) -> Result<String, String> {
    copy_summary_inner(&st, fight_id)
}

#[tauri::command]
fn copy_text(text: String) -> Result<(), String> {
    winsys::set_clipboard(&text)
}

#[tauri::command]
fn reset_fight(st: St<'_>) {
    let mut c = st.lock();
    if let Some(f) = c.pipeline.engine.clear() {
        c.fight_finished(f);
    }
}

#[tauri::command]
fn clear_session(st: St<'_>) {
    let mut c = st.lock();
    c.pipeline.engine.clear();
    c.pipeline.engine.clear_history();
}

#[tauri::command]
fn get_events(st: St<'_>) -> String {
    st.lock().events_view().to_string()
}

#[tauri::command]
fn set_name(st: St<'_>, kind: String, id: u32, name: String) -> Result<String, String> {
    st.lock().set_name(&kind, id, &name)
}

/// Opens a gaming.tools page for a monster ("npc") or skill.
#[tauri::command]
fn open_lookup(kind: String, id: u32) -> Result<(), String> {
    let url = match kind.as_str() {
        "npc" => meter_core::gamingtools::npc_url(id),
        "skill" => meter_core::gamingtools::skill_url(meter_core::engine::Names::skill_name_key(id)),
        "site" => meter_core::gamingtools::SITE.to_string(),
        _ => return Err("unknown kind".into()),
    };
    winsys::open_url(&url)
}

/// Update check: status for Settings, "Check now", hide a version, and open
/// the release page (only the project's own GitHub releases).
#[tauri::command]
fn update_status(st: St<'_>) -> String {
    st.lock().update_view()
}

#[tauri::command]
fn check_updates_now(st: St<'_>) {
    st.lock().update_wake = true;
}

#[tauri::command]
fn dismiss_update(st: St<'_>, version: String) {
    let mut c = st.lock();
    c.settings.skipped_update = version;
    c.settings_dirty = true;
}

#[tauri::command]
fn open_release(url: String) -> Result<(), String> {
    if !url.starts_with(state::RELEASES_URL) {
        return Err("not a Daeva Meter release page".into());
    }
    winsys::open_url(&url)
}

/// Opens one of the meter's own pages (Settings → Updates, About): a fixed
/// list, never a URL from the page.
#[tauri::command]
fn open_link(which: String) -> Result<(), String> {
    let url = match which.as_str() {
        "github" => "https://github.com/fhazal/DAEVA-dps-meter",
        "releases" => state::RELEASES_URL,
        "discord" => "https://discord.gg/TCmm8cH3GE",
        "site" => "https://aion2.bajubesi.org",
        "aion2gg" => "https://aion2.gg",
        _ => return Err("unknown link".into()),
    };
    winsys::open_url(url)
}

#[tauri::command]
fn get_alerts(st: St<'_>) -> String {
    let now = state::now_unix();
    let mut c = st.lock();
    // Each ask is also the alert window's sign of life (see toast_positioner).
    c.toast_health.seen = now;
    c.alerts_view(now).to_string()
}

/// The alert window put these alerts on screen; they stop waiting.
#[tauri::command]
fn alert_shown(st: St<'_>, keys: Vec<String>) {
    st.lock().events.acked.extend(keys);
}

#[tauri::command]
fn refresh_names(st: St<'_>) {
    st.lock().site_wake = true;
}

#[tauri::command]
fn get_icons(st: St<'_>) -> String {
    st.lock().icons_view()
}

#[tauri::command]
fn get_names(st: St<'_>) -> String {
    st.lock().names_view().to_string()
}

/// Where the banners are in the alert window (for hover and dragging).
#[tauri::command]
fn toast_rects(st: St<'_>, rects: Vec<[f64; 4]>) {
    st.lock().toast_rects = rects.iter().map(|r| (r[0], r[1], r[2], r[3])).collect();
}

/// For "Show a test alert": is the alert window there, where, and did it
/// draw a banner?
#[tauri::command]
fn alert_debug(app: AppHandle, st: St<'_>) -> String {
    let drawn = st.lock().toast_rects.len();
    let w = app.get_webview_window("toast");
    let (visible, pos, size) = match &w {
        Some(t) => (t.is_visible().unwrap_or(false), t.outer_position().ok().map(|p| (p.x, p.y)), t.outer_size().ok().map(|s| (s.width, s.height))),
        None => (false, None, None),
    };
    meter_core::json::Obj::new()
        .set("exists", w.is_some())
        .set("visible", visible)
        .set("drawn", drawn)
        .set("x", pos.map(|p| p.0 as f64))
        .set("y", pos.map(|p| p.1 as f64))
        .set("w", size.map(|s| s.0 as f64))
        .set("h", size.map(|s| s.1 as f64))
        .build()
        .to_string()
}

/// Starts dragging a banner. The alert window first shifts so the banner
/// sits at its top edge (custom spots stack downwards), then follows the
/// mouse; the positioner saves the spot when it stops moving.
#[tauri::command]
fn alert_drag(st: St<'_>, window: WebviewWindow, top: f64) -> Result<(), String> {
    if window.label() != "toast" {
        return Err("not the alert window".into());
    }
    let scale = window.scale_factor().unwrap_or(1.0);
    if let Ok(p) = window.outer_position() {
        let y = p.y + ((top - 5.0) * scale) as i32;
        let _ = window.set_position(tauri::PhysicalPosition::new(p.x, y));
    }
    {
        let mut c = st.lock();
        c.toast_anchor = "top";
        let now = state::now_unix();
        c.alert_drag = Some((now, None, now));
    }
    window.start_dragging().map_err(|e| e.to_string())
}

#[tauri::command]
fn test_alert(st: St<'_>) {
    st.lock().test_alert();
}

#[tauri::command]
fn get_settings(st: St<'_>) -> Settings {
    st.lock().settings.clone()
}

#[tauri::command]
fn save_settings(app: AppHandle, st: St<'_>, settings: Settings) -> Vec<String> {
    let restart_hotkeys;
    let restart_capture;
    let new_layout;
    {
        let mut c = st.lock();
        restart_hotkeys = format!("{:?}", c.settings.hotkeys) != format!("{:?}", settings.hotkeys);
        restart_capture = c.settings.device_filter != settings.device_filter || c.settings.ignore_ports != settings.ignore_ports;
        new_layout = (c.settings.layout != settings.layout).then(|| settings.layout.clone());
        // The layout switch also resizes the window, below.
        let mut settings = settings.clone();
        settings.layout = c.settings.layout.clone();
        c.apply_settings(settings);
        c.save_settings();
    }
    if let Some(l) = new_layout {
        set_layout_inner(&app, &st, Some(&l));
        st.lock().save_settings();
    }
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.set_always_on_top(settings.always_on_top);
        // Click-through itself is applied by the hover thread in main().
    }
    if restart_capture {
        st.capture_gen.fetch_add(1, Ordering::SeqCst);
    }
    if restart_hotkeys {
        start_hotkeys(&app, &st);
    }
    st.lock().status.hotkey_errors.clone()
}

fn toggle_click_through_inner(app: &AppHandle, shared: &Shared) -> bool {
    let v = {
        let mut c = shared.lock();
        c.settings.click_through = !c.settings.click_through;
        c.settings_dirty = true;
        c.settings.click_through
    };
    // The hover thread in main() applies it within 80 ms; setting it here as
    // well left the thread's cached state wrong, so the title bar (and the
    // lock button) stayed unclickable until the mouse left and came back.
    let _ = app;
    v
}

#[tauri::command]
fn toggle_click_through(app: AppHandle, st: St<'_>) -> bool {
    toggle_click_through_inner(&app, &st)
}

/// The Solo window: the saved size, or one that fits the card.
fn solo_size(w: &tauri::WebviewWindow, s: &Settings) -> (u32, u32) {
    if let Some((width, height)) = s.solo_size {
        return (width.clamp(200, 4000), height.clamp(60, 3000));
    }
    let scale = w.scale_factor().unwrap_or(1.0) * s.scale;
    ((476.0 * scale) as u32, (180.0 * scale) as u32)
}

/// Switches the overlay to `layout` ("classic" / "solo"; None = the other
/// one) and gives the window that layout's size. The position stays.
fn set_layout_inner(app: &AppHandle, shared: &Shared, layout: Option<&str>) -> String {
    let s = {
        let mut c = shared.lock();
        let next = match layout {
            Some(l @ ("classic" | "solo")) => l.to_string(),
            _ if c.settings.layout == "solo" => "classic".into(),
            _ => "solo".into(),
        };
        c.settings.layout = next;
        c.settings_dirty = true;
        c.settings.clone()
    };
    if let Some(w) = app.get_webview_window("main") {
        let size = if s.layout == "solo" {
            Some(solo_size(&w, &s))
        } else {
            s.window.map(|v| (v.2, v.3)).or_else(|| {
                let scale = w.scale_factor().unwrap_or(1.0);
                Some(((380.0 * scale) as u32, (320.0 * scale) as u32))
            })
        };
        if let Some((width, height)) = size {
            let _ = w.set_size(tauri::PhysicalSize::new(width, height));
        }
    }
    s.layout
}

/// Ctrl + mouse wheel over the meter: the overlay's Scale, 70-160%.
#[tauri::command]
fn set_scale(st: St<'_>, scale: f64) -> f64 {
    let mut c = st.lock();
    c.settings.scale = (scale * 20.0).round().clamp(14.0, 32.0) / 20.0;
    c.settings_dirty = true;
    c.settings.scale
}

#[tauri::command]
fn set_layout(app: AppHandle, st: St<'_>, layout: Option<String>) -> String {
    set_layout_inner(&app, &st, layout.as_deref())
}

#[tauri::command]
fn set_demo(st: St<'_>, on: bool) {
    st.lock().set_demo(on);
}

#[tauri::command]
fn research_start(st: St<'_>, label: String) -> Result<String, String> {
    st.lock().research_start(&label)
}

#[tauri::command]
fn research_stop(st: St<'_>) -> Result<String, String> {
    st.lock().research_stop()
}

#[tauri::command]
fn research_marker(st: St<'_>, label: String) -> Result<usize, String> {
    st.lock().research_marker(&label)
}

#[tauri::command]
fn get_research(st: St<'_>) -> String {
    st.lock().research_view().to_string()
}

fn reveal(path: &str) {
    let _ = std::process::Command::new("explorer").arg(format!("/select,{path}")).spawn();
}

#[tauri::command]
fn report_problem(st: St<'_>, note: Option<String>) -> Result<String, String> {
    let p = st.lock().report_problem(note.as_deref().unwrap_or(""))?;
    reveal(&p);
    Ok(p)
}

#[tauri::command]
fn take_uploads(st: St<'_>) -> Vec<String> {
    std::mem::take(&mut st.lock().uploads)
}

#[tauri::command]
fn get_loot(st: St<'_>) -> String {
    let c = st.lock();
    let e = &c.pipeline.engine;
    meter_core::json::Value::Arr(e.loot_log.iter().rev().map(|l| e.loot_json(l)).collect()).to_string()
}

#[tauri::command]
fn restart_capture(st: St<'_>) {
    st.capture_gen.fetch_add(1, Ordering::SeqCst);
}

#[tauri::command]
fn reload_definitions(st: St<'_>) -> String {
    let mut c = st.lock();
    c.reload_definitions();
    meter_core::json::Obj::new().set("defs", c.status.defs_name.as_str()).set("error", c.status.defs_error.clone()).set("names_error", c.status.names_error.clone()).build().to_string()
}

#[tauri::command]
fn open_folder(st: St<'_>, which: String) -> Result<(), String> {
    let p = {
        let c = st.lock();
        match which.as_str() {
            "history" => c.paths.history.clone(),
            "research" => c.paths.research.clone(),
            "definitions" => c.paths.data.join("definitions"),
            _ => c.paths.data.clone(),
        }
    };
    let _ = std::fs::create_dir_all(&p);
    std::process::Command::new("explorer").arg(p).spawn().map(|_| ()).map_err(|e| e.to_string())
}

#[tauri::command]
fn window_params(window: WebviewWindow, st: St<'_>) -> String {
    st.lock().window_params.get(window.label()).cloned().unwrap_or_else(|| "{}".into())
}

#[tauri::command]
fn win_action(app: AppHandle, window: WebviewWindow, action: String) -> Result<(), String> {
    match action.as_str() {
        "close" => {
            if window.label() == "main" {
                if let Some(s) = app.try_state::<Arc<Shared>>() {
                    s.lock().save_settings();
                }
                app.exit(0);
                Ok(())
            } else {
                // Side windows are hidden, not destroyed: re-creating a window
                // under the same label while the old one is still being torn
                // down left the new one without IPC ("failed to acquire webview
                // reference") and with a dead close button.
                window.hide().map_err(|e| e.to_string())
            }
        }
        "minimize" => window.minimize().map_err(|e| e.to_string()),
        "drag" => window.start_dragging().map_err(|e| e.to_string()),
        "hide" => window.hide().map_err(|e| e.to_string()),
        _ => Err(format!("unknown action {action}")),
    }
}

/// Windows must be created from an async command on Windows (sync commands
/// run on the main thread and would deadlock).
#[tauri::command]
async fn open_window(app: AppHandle, kind: String, params: Option<String>) -> Result<(), String> {
    let (title, w, h) = match kind.as_str() {
        "detail" => ("Daeva Meter — Details", 780.0, 640.0),
        "history" => ("Daeva Meter — History", 640.0, 560.0),
        "settings" => ("Daeva Meter — Settings", 780.0, 640.0),
        "research" => ("Daeva Meter — Research", 900.0, 700.0),
        "names" => ("Daeva Meter — Names", 620.0, 640.0),
        _ => return Err(format!("unknown window {kind}")),
    };
    if let Some(s) = app.try_state::<Arc<Shared>>() {
        s.lock().window_params.insert(kind.clone(), params.unwrap_or_else(|| "{}".into()));
    }
    if let Some(win) = app.get_webview_window(&kind) {
        let _ = win.eval("window.__reloadParams && window.__reloadParams()");
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
        return Ok(());
    }
    WebviewWindowBuilder::new(&app, &kind, WebviewUrl::App(format!("{kind}.html").into()))
        .title(title)
        .inner_size(w, h)
        .min_inner_size(360.0, 240.0)
        .decorations(false)
        .transparent(true)
        // Like the meter: Windows' own shadow and 1px frame are square and
        // showed around the rounded window.
        .shadow(false)
        .always_on_top(true)
        .maximizable(false)
        .build()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------- hotkeys

fn start_hotkeys(app: &AppHandle, shared: &Arc<Shared>) {
    if let Some(old) = shared.hotkeys.lock().unwrap_or_else(|e| e.into_inner()).take() {
        old.stop();
    }
    let bindings = shared.lock().settings.hotkey_bindings();
    let app2 = app.clone();
    let sh = shared.clone();
    let (t, failed) = winsys::HotkeyThread::start(bindings, move |action| match action {
        "click_through" => {
            toggle_click_through_inner(&app2, &sh);
        }
        "reset" => {
            let mut c = sh.lock();
            if let Some(f) = c.pipeline.engine.clear() {
                c.fight_finished(f);
            }
        }
        "hide" => {
            if let Some(w) = app2.get_webview_window("main") {
                if w.is_visible().unwrap_or(true) {
                    let _ = w.hide();
                } else {
                    let _ = w.show();
                }
            }
        }
        "marker" => {
            // One key, nothing to type: "something looks wrong here".
            let mut c = sh.lock();
            if c.research_marker("⚑ bug here (marker key)").is_ok() {
                winsys::play_wav(meter_core::chime::chime_wav("soon", (c.settings.alerts.volume / 100.0) as f32 * 0.4));
            }
        }
        "layout" => {
            set_layout_inner(&app2, &sh, None);
        }
        "copy" => {
            let _ = copy_summary_inner(&sh, None);
        }
        "report" => {
            // In game: save quietly (a chime and a line on the meter), no
            // Explorer window popping up over the game.
            let mut c = sh.lock();
            if c.report_problem("hotkey").is_ok() {
                winsys::play_wav(meter_core::chime::chime_wav("soon", (c.settings.alerts.volume / 100.0) as f32 * 0.6));
            }
        }
        _ => {}
    });
    shared.lock().status.hotkey_errors = failed;
    *shared.hotkeys.lock().unwrap_or_else(|e| e.into_inner()) = Some(t);
}

// ---------------------------------------------------------------- main

/// Keeps the alert window next to the meter, at a fixed screen spot, or where
/// the user dragged it; and makes it catch the mouse only over a banner while
/// the meter is unlocked (locked = fully click-through, as in fights).
fn toast_positioner(handle: AppHandle, sh: Arc<Shared>) {
    let mut last: Option<(i32, i32, u32)> = None;
    let mut ignoring: Option<bool> = None;
    let mut tick: u32 = 0;
    // No banner on screen and nothing being dragged: the empty window can
    // follow the meter at a gentler pace.
    let mut idle = false;
    let mut banner_up = false;
    let started = state::now_unix();
    let (mut nudged, mut reloaded) = (0.0f64, 0.0f64);
    loop {
        std::thread::sleep(std::time::Duration::from_millis(if idle { 250 } else { 80 }));
        tick = tick.wrapping_add(1);
        let (Some(t), Some(m)) = (handle.get_webview_window("toast"), handle.get_webview_window("main")) else { continue };

        // The alert window asks for alerts every second or so. In long
        // sessions Windows can slow its page down until it misses banners
        // (0.5.4-test.1: nothing drawn until a restart). An alert waiting
        // more than 2 s wakes it directly; no sign of life for 30 s reloads it.
        {
            let now = state::now_unix();
            let mut c = sh.lock();
            let seen = if c.toast_health.seen > 0.0 { c.toast_health.seen } else { started };
            let quiet = now - seen;
            if quiet > 30.0 && now - reloaded > 60.0 {
                reloaded = now;
                c.toast_fix(now, &format!("no reply for {quiet:.0} s"), true);
                drop(c);
                let _ = t.reload();
            } else if quiet > 2.0 && now - nudged > 3.0 && c.settings.alerts.enabled && c.events.any_pending(now) {
                nudged = now;
                c.toast_fix(now, &format!("alert waiting, no reply for {quiet:.1} s"), false);
                drop(c);
                let _ = t.eval("window.__daevaPoll && window.__daevaPoll()");
            }
        }
        let scale = t.scale_factor().unwrap_or(1.0);
        let px = |v: f64| (v * scale) as i32;
        let (Ok(tp), Ok(ts)) = (t.outer_position(), t.outer_size()) else { continue };

        // Mouse: only over a banner, only while the meter is unlocked.
        let (locked, rects, dragging) = {
            let c = sh.lock();
            (c.settings.click_through, c.toast_rects.clone(), c.alert_drag.is_some())
        };
        idle = rects.is_empty() && !dragging;
        let over = !locked
            && winsys::cursor_pos().map_or(false, |(cx, cy)| {
                rects.iter().any(|(x, y, w, h)| {
                    let (l, tt) = (tp.x + px(*x), tp.y + px(*y));
                    cx >= l && cx < l + px(*w) && cy >= tt && cy < tt + px(*h)
                })
            });
        // A banner up: keep its window above the game, at once and then
        // about once a second.
        let raise = !rects.is_empty() && (!banner_up || tick % 12 == 0);
        banner_up = !rects.is_empty();
        if raise {
            #[cfg(windows)]
            if let Ok(h) = t.hwnd() {
                winsys::keep_on_top(h.0 as isize);
            }
        }
        let ignore = !(over || dragging);
        if ignoring != Some(ignore) || tick % 25 == 0 {
            if t.set_ignore_cursor_events(ignore).is_ok() {
                ignoring = Some(ignore);
            }
        }

        // Dragging: leave the window alone; save the spot once it rests.
        if dragging {
            let now = state::now_unix();
            let mut c = sh.lock();
            let Some((started, lastp, moved_at)) = c.alert_drag else { continue };
            let here = (tp.x, tp.y);
            if lastp != Some(here) {
                c.alert_drag = Some((started, Some(here), now));
            } else if (lastp.is_some() && now - moved_at > 0.8) || now - started > 20.0 {
                c.alert_drag = None;
                if let Some(mon) = t.current_monitor().ok().flatten() {
                    let (mx, my) = (mon.position().x, mon.position().y);
                    let (mw, mh) = (mon.size().width as i32, mon.size().height as i32);
                    let fx = ((tp.x - mx) as f64 / (mw - ts.width as i32).max(1) as f64).clamp(0.0, 1.0);
                    let fy = ((tp.y - my) as f64 / (mh - ts.height as i32).max(1) as f64).clamp(0.0, 1.0);
                    c.settings.alerts.custom = Some((fx, fy));
                    c.settings.alerts.custom_monitor = mon.name().cloned().unwrap_or_default();
                    c.settings.alerts.position = "custom".into();
                    c.save_settings();
                }
                last = None;
            }
            continue;
        }

        let (pos, ox, oy, custom, cmon) = {
            let c = sh.lock();
            let a = &c.settings.alerts;
            (a.position.clone(), a.offset_x, a.offset_y, a.custom, a.custom_monitor.clone())
        };
        let (Ok(mp), Ok(ms)) = (m.outer_position(), m.outer_size()) else { continue };
        let meter_mon = m.current_monitor().ok().flatten().or_else(|| m.primary_monitor().ok().flatten());
        let (sx, sy, sw) = meter_mon.as_ref().map(|x| (x.position().x, x.position().y, x.size().width as i32)).unwrap_or((0, 0, 1920));
        let on_meter = pos == "above_meter" || pos == "below_meter";
        // Same width as the meter when attached to it, otherwise 360 px.
        let width = if on_meter { ms.width.max(px(260.0) as u32) } else { px(360.0) as u32 };
        let (tw, th) = (width as i32, ts.height as i32);
        let gap = px(6.0);
        let below = (mp.x, mp.y + ms.height as i32 + gap);
        let (mut x, mut y, anchor) = match (pos.as_str(), custom) {
            ("custom", Some((fx, fy))) => {
                // The monitor it was placed on (by name), else the meter's.
                let mons = t.available_monitors().unwrap_or_default();
                let mon = mons.iter().find(|x| !cmon.is_empty() && x.name().map_or(false, |n| *n == cmon)).cloned().or(meter_mon.clone());
                let (mx, my, mw, mh) = mon.map(|x| (x.position().x, x.position().y, x.size().width as i32, x.size().height as i32)).unwrap_or((0, 0, 1920, 1080));
                (mx + ((mw - tw).max(0) as f64 * fx) as i32, my + ((mh - th).max(0) as f64 * fy) as i32, "top")
            }
            ("below_meter", _) => (below.0, below.1, "top"),
            ("screen_top_center", _) => (sx + (sw - tw) / 2, sy + px(60.0), "top"),
            // AION 2's minimap sits top-right; stay just to its left.
            ("screen_top_right", _) => (sx + sw - tw - px(300.0), sy + px(16.0), "top"),
            ("screen_top_left", _) => (sx + px(16.0), sy + px(16.0), "top"),
            _ => {
                let above = mp.y - th - gap;
                if above >= sy { (mp.x, above, "bottom") } else { (below.0, below.1, "top") }
            }
        };
        if pos != "custom" {
            x += px(ox);
            y += px(oy);
        }
        sh.lock().toast_anchor = anchor;
        if last != Some((x, y, width)) {
            if ts.width != width {
                let _ = t.set_size(tauri::PhysicalSize::new(width, ts.height));
            }
            if t.set_position(tauri::PhysicalPosition::new(x, y)).is_ok() {
                last = Some((x, y, width));
            }
        }
    }
}

fn main() {
    tauri::Builder::default()
        // One meter at a time: opening it again brings the running one forward
        // (two copies would read the game twice).
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.show();
                let _ = w.set_focus();
            }
            if let Some(sh) = app.try_state::<Arc<Shared>>() {
                sh.lock().opened_again = Some(state::now_unix());
            }
        }))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let data = app.path().app_data_dir()?;
            let resources = app.path().resource_dir()?;
            let shared = Shared::new(data, resources);
            shared.lock().app_version = app.package_info().version.to_string();
            app.manage(shared.clone());
            state::start_threads(shared.clone());
            selfupdate::start(app.handle().clone(), shared.clone());
            start_hotkeys(app.handle(), &shared);
            // While click-through is on, keep only the footer (where the lock
            // button sits) clickable, so the lock can be clicked off again but
            // the meter cannot be dragged or clicked anywhere else.
            {
                let handle = app.handle().clone();
                let sh = shared.clone();
                std::thread::spawn(move || {
                    let mut applied: Option<bool> = None;
                    let mut sized: Option<bool> = None;
                    let mut tick: u32 = 0;
                    loop {
                        std::thread::sleep(std::time::Duration::from_millis(80));
                        let Some(w) = handle.get_webview_window("main") else { continue };
                        let ct = sh.lock().settings.click_through;
                        let mut ignore = ct;
                        // Locked: no resizing from the window edges either.
                        if sized != Some(!ct) && w.set_resizable(!ct).is_ok() {
                            sized = Some(!ct);
                        }
                        if ct {
                            // The bottom line grows with Scale (Solo is drawn a bit
                            // larger), so the strip does too: the whole lock button
                            // stays clickable at any size.
                            let (user, solo) = { let c = sh.lock(); (c.settings.scale, c.settings.layout == "solo") };
                            if let (Some((cx, cy)), Ok(pos), Ok(size), Ok(scale)) = (winsys::cursor_pos(), w.outer_position(), w.outer_size(), w.scale_factor()) {
                                let foot = ((if solo { 28.0 } else { 24.0 } * user).max(26.0) * scale) as i32;
                                let bottom = pos.y + size.height as i32;
                                let inside_x = cx >= pos.x && cx < pos.x + size.width as i32;
                                if inside_x && cy >= bottom - foot && cy < bottom {
                                    ignore = false;
                                }
                            }
                        }
                        // Re-assert every ~2 s too, in case anything else changed it.
                        tick = tick.wrapping_add(1);
                        if applied != Some(ignore) || tick % 25 == 0 {
                            if w.set_ignore_cursor_events(ignore).is_ok() {
                                applied = Some(ignore);
                            }
                        }
                    }
                });
            }
            // Event banners live in their own click-through window, so they
            // never cover the meter; it follows the meter or sits where the
            // user chose (Settings → Event alerts → Show alerts).
            match WebviewWindowBuilder::new(app.handle(), "toast", WebviewUrl::App("toast.html".into()))
                .title("Daeva Meter — Alerts")
                .inner_size(360.0, 280.0)
                .decorations(false)
                .transparent(true)
                .always_on_top(true)
                .skip_taskbar(true)
                .resizable(false)
                .shadow(false)
                .focused(false)
                .build()
            {
                Ok(t) => {
                    let _ = t.set_ignore_cursor_events(true);
                    let handle = app.handle().clone();
                    let sh = shared.clone();
                    std::thread::spawn(move || toast_positioner(handle, sh));
                }
                Err(e) => eprintln!("alert window: {e}"),
            }
            if let Some(w) = app.get_webview_window("main") {
                let s = shared.lock().settings.clone();
                if let Some((x, y, width, height)) = s.window {
                    let (mut width, mut height) = (width.clamp(200, 4000), height.clamp(100, 3000));
                    // A size saved while the window was maximised (fills a
                    // whole monitor): start at the normal size instead.
                    let huge = w.available_monitors().map(|ms| {
                        ms.iter().any(|m| width + 40 >= m.size().width && height + 80 >= m.size().height)
                    });
                    if huge.unwrap_or(false) {
                        let scale = w.scale_factor().unwrap_or(1.0);
                        width = (380.0 * scale) as u32;
                        height = (320.0 * scale) as u32;
                    }
                    if s.layout == "solo" {
                        (width, height) = solo_size(&w, &s);
                    }
                    // Only restore a position that is (mostly) on a monitor;
                    // otherwise keep the default spot so the meter is visible.
                    let on_screen = w.available_monitors().map(|ms| {
                        ms.iter().any(|m| {
                            let (mx, my) = (m.position().x, m.position().y);
                            let (mw, mh) = (m.size().width as i32, m.size().height as i32);
                            x + 80 > mx && x < mx + mw - 80 && y >= my - 10 && y < my + mh - 40
                        })
                    });
                    if on_screen.unwrap_or(false) {
                        let _ = w.set_position(tauri::PhysicalPosition::new(x, y));
                    } else {
                        let _ = w.center();
                    }
                    let _ = w.set_size(tauri::PhysicalSize::new(width, height));
                } else if s.layout == "solo" {
                    let (width, height) = solo_size(&w, &s);
                    let _ = w.set_size(tauri::PhysicalSize::new(width, height));
                }
                let _ = w.set_always_on_top(s.always_on_top);
                let _ = w.set_ignore_cursor_events(s.click_through);
                let _ = w.set_maximizable(false);
                if w.is_maximized().unwrap_or(false) {
                    let _ = w.unmaximize();
                }
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Never stay maximised (Windows snap to the top edge, or a stray
            // double-click): put the window back to its normal size.
            if let tauri::WindowEvent::Resized(_) = event {
                if window.is_maximized().unwrap_or(false) {
                    let _ = window.unmaximize();
                    return;
                }
            }
            if window.label() != "main" {
                // Alt+F4 / taskbar close on a side window: hide it instead.
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    if window.label() != "toast" {
                        api.prevent_close();
                        let _ = window.hide();
                    }
                }
                return;
            }
            let Some(shared) = window.app_handle().try_state::<Arc<Shared>>() else { return };
            match event {
                tauri::WindowEvent::Moved(p) => {
                    // Minimising parks the window at -32000,-32000 on Windows;
                    // saving that made the meter reopen off-screen.
                    if p.x <= -20000 || p.y <= -20000 || window.is_minimized().unwrap_or(false) {
                        return;
                    }
                    let mut c = shared.lock();
                    let (w, h) = c.settings.window.map(|x| (x.2, x.3)).unwrap_or((380, 320));
                    c.settings.window = Some((p.x, p.y, w, h));
                    c.settings_dirty = true;
                }
                tauri::WindowEvent::Resized(s) => {
                    if s.width == 0 || s.height == 0 {
                        return; // minimised
                    }
                    let mut c = shared.lock();
                    // Each layout keeps its own size.
                    if c.settings.layout == "solo" {
                        c.settings.solo_size = Some((s.width, s.height));
                    } else {
                        let (x, y) = c.settings.window.map(|v| (v.0, v.1)).unwrap_or((100, 100));
                        c.settings.window = Some((x, y, s.width, s.height));
                    }
                    c.settings_dirty = true;
                }
                tauri::WindowEvent::CloseRequested { .. } => {
                    shared.lock().save_settings();
                    // Closing the meter quits everything (alert window and
                    // hidden side windows included).
                    window.app_handle().exit(0);
                }
                _ => {}
            }
        })
        .invoke_handler(tauri::generate_handler![
            update_status,
            check_updates_now,
            open_link,
            dismiss_update,
            open_release,
            get_snapshot,
            get_detail,
            get_history,
            get_fight_record,
            copy_summary,
            copy_text,
            reset_fight,
            clear_session,
            get_settings,
            save_settings,
            toggle_click_through,
            set_layout,
            set_scale,
            set_demo,
            research_start,
            research_stop,
            research_marker,
            get_research,
            take_uploads,
            report_problem,
            get_loot,
            restart_capture,
            reload_definitions,
            open_folder,
            window_params,
            win_action,
            open_window,
            get_events,
            test_alert,
            alert_shown,
            toast_rects,
            alert_debug,
            alert_drag,
            set_name,
            get_names,
            open_lookup,
            get_icons,
            refresh_names,
            get_alerts,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Daeva Meter");
}
