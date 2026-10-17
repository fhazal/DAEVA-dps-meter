#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod npcap;
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
    meter_core::json::Obj::new().set("session", c.pipeline.engine.history_list()).set("saved", c.saved_fights()).build().to_string()
}

#[tauri::command]
fn get_fight_record(st: St<'_>, fight_id: Option<u64>, file: Option<String>) -> Result<String, String> {
    let c = st.lock();
    if let Some(file) = file {
        return state::read_saved_fight(&c.paths.history, &file);
    }
    let e = &c.pipeline.engine;
    let f = match fight_id {
        Some(id) => e.fight(id),
        None => e.display_fight(),
    }
    .ok_or("fight not found")?;
    Ok(e.fight_record(f).to_string())
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
    if let Some(f) = c.pipeline.engine.finish() {
        c.fight_finished(f);
    }
}

#[tauri::command]
fn clear_session(st: St<'_>) {
    let mut c = st.lock();
    c.pipeline.engine.finish();
    c.pipeline.engine.clear_history();
}

#[tauri::command]
fn get_events(st: St<'_>) -> String {
    st.lock().events_view().to_string()
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
    {
        let mut c = st.lock();
        restart_hotkeys = format!("{:?}", c.settings.hotkeys) != format!("{:?}", settings.hotkeys);
        restart_capture = c.settings.device_filter != settings.device_filter || c.settings.ignore_ports != settings.ignore_ports;
        c.apply_settings(settings.clone());
        c.save_settings();
    }
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.set_always_on_top(settings.always_on_top);
        let _ = w.set_ignore_cursor_events(settings.click_through);
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
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.set_ignore_cursor_events(v);
    }
    v
}

#[tauri::command]
fn toggle_click_through(app: AppHandle, st: St<'_>) -> bool {
    toggle_click_through_inner(&app, &st)
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
                window.close().map_err(|e| e.to_string())
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
        "settings" => ("Daeva Meter — Settings", 560.0, 680.0),
        "research" => ("Daeva Meter — Research", 900.0, 700.0),
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
        .always_on_top(true)
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
            if let Some(f) = c.pipeline.engine.finish() {
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
            let _ = sh.lock().research_marker("hotkey marker");
        }
        "copy" => {
            let _ = copy_summary_inner(&sh, None);
        }
        "report" => {
            let r = sh.lock().report_problem("hotkey");
            if let Ok(p) = r {
                reveal(&p);
            }
        }
        _ => {}
    });
    shared.lock().status.hotkey_errors = failed;
    *shared.hotkeys.lock().unwrap_or_else(|e| e.into_inner()) = Some(t);
}

// ---------------------------------------------------------------- main

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let data = app.path().app_data_dir()?;
            let resources = app.path().resource_dir()?;
            let shared = Shared::new(data, resources);
            app.manage(shared.clone());
            state::start_threads(shared.clone());
            start_hotkeys(app.handle(), &shared);
            // While click-through is on, keep the title bar clickable when the
            // mouse is over it, so the lock can always be clicked off again.
            {
                let handle = app.handle().clone();
                let sh = shared.clone();
                std::thread::spawn(move || {
                    let mut applied: Option<bool> = None;
                    loop {
                        std::thread::sleep(std::time::Duration::from_millis(80));
                        let Some(w) = handle.get_webview_window("main") else { continue };
                        let ct = sh.lock().settings.click_through;
                        let mut ignore = ct;
                        if ct {
                            if let (Some((cx, cy)), Ok(pos), Ok(size), Ok(scale)) = (winsys::cursor_pos(), w.outer_position(), w.outer_size(), w.scale_factor()) {
                                let bar = (34.0 * scale) as i32;
                                let inside_x = cx >= pos.x && cx < pos.x + size.width as i32;
                                let inside_bar = cy >= pos.y && cy < pos.y + bar;
                                if inside_x && inside_bar {
                                    ignore = false;
                                }
                            }
                        }
                        if applied != Some(ignore) {
                            if w.set_ignore_cursor_events(ignore).is_ok() {
                                applied = Some(ignore);
                            }
                        }
                    }
                });
            }
            if let Some(w) = app.get_webview_window("main") {
                let s = shared.lock().settings.clone();
                if let Some((x, y, width, height)) = s.window {
                    let _ = w.set_position(tauri::PhysicalPosition::new(x, y));
                    let _ = w.set_size(tauri::PhysicalSize::new(width.max(200), height.max(100)));
                }
                let _ = w.set_always_on_top(s.always_on_top);
                let _ = w.set_ignore_cursor_events(s.click_through);
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() != "main" {
                return;
            }
            let Some(shared) = window.app_handle().try_state::<Arc<Shared>>() else { return };
            match event {
                tauri::WindowEvent::Moved(p) => {
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
                    let (x, y) = c.settings.window.map(|v| (v.0, v.1)).unwrap_or((100, 100));
                    c.settings.window = Some((x, y, s.width, s.height));
                    c.settings_dirty = true;
                }
                tauri::WindowEvent::CloseRequested { .. } => {
                    shared.lock().save_settings();
                }
                _ => {}
            }
        })
        .invoke_handler(tauri::generate_handler![
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running Daeva Meter");
}
