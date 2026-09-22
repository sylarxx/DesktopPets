use crate::runtime_health::{Repair, RuntimeHealth, LABELS};
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Mutex,
};
use std::time::Instant;
use tauri::{Emitter, Manager, PhysicalPosition, PhysicalSize, Position, Size};

#[derive(Clone, Serialize)]
pub struct RuntimeState {
    epoch: u64,
    interactive: bool,
    recovered: bool,
    visible: bool,
}

#[derive(Clone)]
struct RecoveryWindow {
    position: PhysicalPosition<i32>,
    size: PhysicalSize<u32>,
    visible: bool,
    recreate: bool,
    mounted: bool,
}

#[derive(Serialize)]
struct HealthEvent {
    event: &'static str,
    window: String,
    epoch: u64,
    elapsed_ms: u64,
    process_id: u32,
}

pub struct DesktopRuntime {
    health: Mutex<RuntimeHealth>,
    windows: Mutex<BTreeMap<String, RecoveryWindow>>,
    started: Instant,
    scheduled: AtomicBool,
    log: Mutex<Option<mpsc::SyncSender<HealthEvent>>>,
}

impl Default for DesktopRuntime {
    fn default() -> Self {
        Self {
            health: Mutex::new(RuntimeHealth::default()),
            windows: Mutex::new(BTreeMap::new()),
            started: Instant::now(),
            scheduled: AtomicBool::new(false),
            log: Mutex::new(None),
        }
    }
}

impl DesktopRuntime {
    fn now(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }
    pub fn interactive(&self) -> bool {
        self.health.lock().map(|h| h.interactive).unwrap_or(false)
    }

    fn state(&self, label: &str) -> RuntimeState {
        let h = self.health.lock().unwrap();
        let view = &h.views[label];
        RuntimeState {
            epoch: h.epoch,
            interactive: h.interactive,
            recovered: view.recovered,
            visible: view.restore_visible,
        }
    }

    fn record(&self, event: &'static str, label: &str) {
        let epoch = self.health.lock().map(|h| h.epoch).unwrap_or(0);
        if let Ok(sender) = self.log.lock() {
            if let Some(sender) = sender.as_ref() {
                let _ = sender.try_send(HealthEvent {
                    event,
                    window: label.into(),
                    epoch,
                    elapsed_ms: self.now(),
                    process_id: std::process::id(),
                });
            }
        }
    }

    pub fn user_hide(&self) {
        if let Ok(mut windows) = self.windows.lock() {
            for snapshot in windows.values_mut() {
                snapshot.visible = false;
            }
        }
    }

    pub fn rebuilding(&self) -> bool {
        self.windows
            .lock()
            .map(|windows| windows.values().any(|s| s.recreate))
            .unwrap_or(false)
    }
}

pub fn initialize(app: &tauri::AppHandle) {
    // A separate allowlisted log. Never forward legacy diagnostic fields,
    // message bodies, auth URLs or tokens to this writer.
    if let Ok(directory) = app.path().app_log_dir() {
        let (sender, receiver) = mpsc::sync_channel::<HealthEvent>(64);
        *app.state::<DesktopRuntime>().log.lock().unwrap() = Some(sender);
        std::thread::spawn(move || {
            use std::io::Write;
            let _ = std::fs::create_dir_all(&directory);
            let path = directory.join("runtime-health.jsonl");
            for event in receiver {
                if std::fs::metadata(&path)
                    .map(|m| m.len() > 262_144)
                    .unwrap_or(false)
                {
                    let old = directory.join("runtime-health.jsonl.1");
                    let _ = std::fs::remove_file(&old);
                    let _ = std::fs::rename(&path, old);
                }
                if let (Ok(mut file), Ok(line)) = (
                    std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&path),
                    serde_json::to_string(&event),
                ) {
                    let _ = writeln!(file, "{line}");
                }
            }
        });
    }
    app.state::<DesktopRuntime>().record("runtime-start", "");
    #[cfg(windows)]
    crate::windows_session::start(app.clone());
}

pub fn session_changed(app: &tauri::AppHandle, interactive: bool, force: bool) {
    let runtime = app.state::<DesktopRuntime>();
    let changed = runtime
        .health
        .lock()
        .unwrap()
        .session(interactive, runtime.now(), force);
    if changed {
        runtime.record(
            if interactive {
                "session-interactive"
            } else {
                "session-inactive"
            },
            "",
        );
        publish_state(app);
    }
}

fn publish_state(app: &tauri::AppHandle) {
    for label in LABELS {
        let state = app.state::<DesktopRuntime>().state(label);
        let _ = app.emit_to(label, "desktop-runtime-state", state);
    }
}

#[tauri::command]
pub fn desktop_runtime_ready(
    window: tauri::WebviewWindow,
    instance: String,
) -> Result<RuntimeState, String> {
    let app = window.app_handle();
    let runtime = app.state::<DesktopRuntime>();
    let before = runtime.health.lock().unwrap().epoch;
    if !runtime
        .health
        .lock()
        .unwrap()
        .attach(window.label(), &instance, runtime.now())
    {
        return Err("stale renderer".into());
    }
    let state = runtime.state(window.label());
    if state.epoch != before {
        runtime.record("renderer-attached-after-repair", window.label());
        publish_state(app);
    }
    Ok(state)
}

#[tauri::command]
pub fn desktop_runtime_mounted(window: tauri::WebviewWindow, instance: String) -> bool {
    let app = window.app_handle();
    let runtime = app.state::<DesktopRuntime>();
    {
        let mut h = runtime.health.lock().unwrap();
        let Some(view) = h.views.get_mut(window.label()) else {
            return false;
        };
        if view.instance != instance {
            return false;
        }
        view.application_ready = true;
    }
    if let Some(snapshot) = runtime.windows.lock().unwrap().get_mut(window.label()) {
        snapshot.mounted = true;
    }
    // All HWND work is marshalled without holding a health/state mutex. This
    // also avoids a main-thread -> mutex -> synchronous HWND getter deadlock.
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || finish_mounted_windows(&handle));
    true
}

#[tauri::command]
pub fn desktop_runtime_ack(
    window: tauri::WebviewWindow,
    instance: String,
    epoch: u64,
    sequence: u64,
    painted: bool,
) -> bool {
    let runtime = window.state::<DesktopRuntime>();
    let result = runtime.health.lock().unwrap().ack(
        window.label(),
        &instance,
        epoch,
        sequence,
        painted,
        runtime.now(),
    );
    result
}

#[tauri::command]
pub fn request_notification_recovery(window: tauri::WebviewWindow) {
    if window.label() == "mascot" {
        window
            .state::<DesktopRuntime>()
            .health
            .lock()
            .unwrap()
            .request("mascot-notification", false);
    }
}

pub fn register_native_generation(app: &tauri::AppHandle, label: &str) -> u64 {
    let runtime = app.state::<DesktopRuntime>();
    let mut health = runtime.health.lock().unwrap();
    let view = health.views.get_mut(label).unwrap();
    view.native_generation += 1;
    view.native_generation
}

pub fn process_failed(app: &tauri::AppHandle, label: &str, browser: bool, generation: u64) {
    let runtime = app.state::<DesktopRuntime>();
    {
        let mut h = runtime.health.lock().unwrap();
        if h.views
            .get(label)
            .is_none_or(|view| view.native_generation != generation)
        {
            return;
        }
        h.request(label, browser);
        if let Some(view) = h.views.get_mut(label) {
            view.grace_until = runtime.now();
        }
    }
    runtime.record(
        if browser {
            "browser-process-failed"
        } else {
            "renderer-process-failed"
        },
        label,
    );
}

pub fn schedule_tick(app: &tauri::AppHandle) {
    let runtime = app.state::<DesktopRuntime>();
    // There can only be one queued main-thread probe even if its message loop
    // stalls. The independent Windows observer keeps handling lock/resume.
    if runtime.scheduled.swap(true, Ordering::SeqCst) {
        return;
    }
    let handle = app.clone();
    if app
        .run_on_main_thread(move || {
            tick(&handle);
            handle
                .state::<DesktopRuntime>()
                .scheduled
                .store(false, Ordering::SeqCst);
        })
        .is_err()
    {
        runtime.scheduled.store(false, Ordering::SeqCst);
    }
}

fn tick(app: &tauri::AppHandle) {
    let runtime = app.state::<DesktopRuntime>();
    if !runtime.interactive() {
        return;
    }
    finish_recreation(app);
    finish_mounted_windows(app);
    for label in LABELS {
        let Some(window) = app.get_webview_window(label) else {
            continue;
        };
        let pending = runtime.windows.lock().unwrap().get(label).cloned();
        let visible = pending
            .as_ref()
            .map(|s| s.visible)
            .unwrap_or_else(|| window.is_visible().unwrap_or(false));
        let repair = runtime
            .health
            .lock()
            .unwrap()
            .inspect(label, visible, runtime.now());
        if let Some(repair) = repair {
            repair_window(app, &window, repair);
            continue;
        }
        let (state, sequence) = {
            let mut h = runtime.health.lock().unwrap();
            let epoch = h.epoch;
            let interactive = h.interactive;
            let view = h.views.get_mut(label).unwrap();
            view.sequence += 1;
            (
                RuntimeState {
                    epoch,
                    interactive,
                    recovered: view.recovered,
                    visible: view.restore_visible,
                },
                view.sequence,
            )
        };
        let payload = serde_json::json!({
            "epoch": state.epoch, "interactive": state.interactive,
            "recovered": state.recovered, "visible": state.visible,
            "sequence": sequence, "paint": visible && pending.is_none(),
        });
        let _ = window.eval(format!(
            "window.dispatchEvent(new CustomEvent('desktop-runtime-probe',{{detail:{payload}}}))"
        ));
    }
}

fn repair_window(app: &tauri::AppHandle, window: &tauri::WebviewWindow, repair: Repair) {
    let runtime = app.state::<DesktopRuntime>();
    let label = window.label();
    let snapshot = RecoveryWindow {
        position: window
            .outer_position()
            .unwrap_or(PhysicalPosition::new(0, 0)),
        size: window.outer_size().unwrap_or(PhysicalSize::new(120, 104)),
        visible: window.is_visible().unwrap_or(false),
        recreate: false,
        mounted: false,
    };
    {
        let mut windows = runtime.windows.lock().unwrap();
        let saved = windows.entry(label.into()).or_insert(snapshot);
        saved.recreate = repair == Repair::Recreate;
        saved.mounted = false;
    }
    crate::reset_window_for_runtime_recovery(app, label);
    let _ = crate::hide_transparent_window_safely(window);
    runtime.record(
        if repair == Repair::Reload {
            "renderer-reload"
        } else {
            "window-recreate"
        },
        label,
    );
    match repair {
        Repair::Reload => {
            let _ =
                window.set_position(Position::Physical(PhysicalPosition::new(-32_000, -32_000)));
            let _ = crate::show_window_without_activation(window);
            if window.reload().is_err() {
                runtime.record("reload-command-failed", label);
            }
        }
        Repair::Recreate => {
            if window.destroy().is_err() {
                runtime.record("destroy-command-failed", label);
            }
        }
    }
}

fn finish_recreation(app: &tauri::AppHandle) {
    let candidates: Vec<String> = app
        .state::<DesktopRuntime>()
        .windows
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, snapshot)| snapshot.recreate)
        .map(|(label, _)| label.clone())
        .collect();
    for label in candidates {
        if app.get_webview_window(&label).is_some() {
            continue;
        }
        // Consume the creation attempt even on error: no unbounded rebuild loop.
        if let Some(saved) = app
            .state::<DesktopRuntime>()
            .windows
            .lock()
            .unwrap()
            .get_mut(&label)
        {
            saved.recreate = false;
        }
        let Some(config) = app.config().app.windows.iter().find(|c| c.label == label) else {
            continue;
        };
        match tauri::WebviewWindowBuilder::from_config(app, config).and_then(|builder| {
            let builder = if matches!(
                std::env::var("HUALI_AI_VISUAL_SMOKE_FORCE_MOTION").as_deref(),
                Ok("1")
            ) {
                builder.data_directory(
                    std::env::temp_dir()
                        .join(format!("huali-ai-visual-smoke-{}", std::process::id())),
                )
            } else {
                builder
            };
            builder.build()
        }) {
            Ok(window) => {
                if crate::configure_desktop_window(app, &window).is_err() {
                    app.state::<DesktopRuntime>()
                        .record("window-configure-failed", &label);
                }
                let _ = window.set_ignore_cursor_events(true);
                let _ = window
                    .set_position(Position::Physical(PhysicalPosition::new(-32_000, -32_000)));
                let _ = crate::show_window_without_activation(&window);
            }
            Err(_) => app
                .state::<DesktopRuntime>()
                .record("window-create-failed", &label),
        }
    }
}

fn finish_mounted_windows(app: &tauri::AppHandle) {
    let runtime = app.state::<DesktopRuntime>();
    if !runtime.interactive() {
        return;
    }
    let ready: Vec<(String, RecoveryWindow)> = {
        let mut snapshots = runtime.windows.lock().unwrap();
        let ready = snapshots
            .iter()
            .filter(|(_, s)| s.mounted)
            .map(|(label, s)| (label.clone(), s.clone()))
            .collect::<Vec<_>>();
        for (label, _) in &ready {
            snapshots.remove(label);
        }
        ready
    };
    if ready.is_empty() {
        return;
    }
    for (label, snapshot) in ready {
        let Some(window) = app.get_webview_window(&label) else {
            continue;
        };
        let _ = crate::hide_transparent_window_safely(&window);
        let _ = window.set_position(Position::Physical(snapshot.position));
        let _ = window.set_size(Size::Physical(snapshot.size));
        if snapshot.visible && (label == "mascot" || label == "panel") {
            let _ = crate::show_interactive_window(&window, false);
        }
        if label == "mascot" {
            let _ = app.emit_to("mascot", crate::MASCOT_NATIVE_REVEALED_EVENT, ());
        }
        runtime.record("renderer-mounted-after-repair", &label);
    }
    runtime.health.lock().unwrap().epoch += 1;
    publish_state(app);
}
