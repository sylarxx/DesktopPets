use crate::runtime_health::{Repair, RuntimeHealth, LABELS};
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::{
    atomic::{AtomicBool, AtomicI8, AtomicU64, Ordering},
    mpsc, Mutex,
};
use std::time::Instant;
use tauri::{Emitter, Manager, PhysicalPosition, PhysicalSize, Position, Size};

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeState {
    epoch: u64,
    interactive: bool,
    recovered: bool,
    visible: bool,
    delivery_generation: u64,
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
    smoke_session: AtomicI8,
    last_resync: AtomicU64,
    log: Mutex<Option<mpsc::SyncSender<HealthEvent>>>,
}

impl Default for DesktopRuntime {
    fn default() -> Self {
        Self {
            health: Mutex::new(RuntimeHealth::default()),
            windows: Mutex::new(BTreeMap::new()),
            started: Instant::now(),
            scheduled: AtomicBool::new(false),
            smoke_session: AtomicI8::new(-1),
            last_resync: AtomicU64::new(0),
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

    fn state(&self, app: &tauri::AppHandle, label: &str) -> RuntimeState {
        let delivery_generation = app
            .state::<crate::MascotSystemNotificationState>()
            .status
            .lock()
            .map(|s| s.client_generation)
            .unwrap_or(0);
        let h = self.health.lock().unwrap();
        let view = &h.views[label];
        RuntimeState {
            epoch: h.epoch,
            interactive: h.interactive,
            recovered: view.recovered,
            visible: view.restore_visible,
            delivery_generation,
        }
    }

    pub fn record(&self, event: &'static str, label: &str) {
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
    let interactive = match runtime.smoke_session.load(Ordering::SeqCst) {
        0 => false,
        1 => true,
        _ => interactive,
    };
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
        let state = app.state::<DesktopRuntime>().state(app, label);
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
    let state = runtime.state(app, window.label());
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

#[tauri::command]
pub fn request_runtime_resync(window: tauri::WebviewWindow) {
    let app = window.app_handle();
    let runtime = app.state::<DesktopRuntime>();
    if window.label() != "mascot" || !runtime.interactive() {
        return;
    }
    let now = runtime.now();
    if runtime
        .last_resync
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |last| {
            (last == 0 || now.saturating_sub(last) >= 15_000).then_some(now.max(1))
        })
        .is_ok()
    {
        session_changed(app, true, true);
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
        let state = runtime.state(app, label);
        let sequence = {
            let mut h = runtime.health.lock().unwrap();
            let view = h.views.get_mut(label).unwrap();
            view.sequence += 1;
            view.sequence
        };
        let payload = serde_json::json!({
            "epoch": state.epoch, "interactive": state.interactive,
            "recovered": state.recovered, "visible": state.visible,
            "deliveryGeneration": state.delivery_generation,
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

// Fault injection is inert in normal launches. It requires both existing release
// smoke opt-ins and a fresh, isolated WebView profile supplied by the Windows QA
// harness. Commands are a closed list; no caller-provided script is evaluated.
fn smoke_enabled() -> bool {
    crate::desktop_release_smoke_nonce().is_some()
        && matches!(
            std::env::var("HUALI_AI_VISUAL_SMOKE_FORCE_MOTION").as_deref(),
            Ok("1")
        )
}

pub fn handle_smoke_command(app: &tauri::AppHandle, arguments: &[String]) -> bool {
    if !smoke_enabled() {
        return false;
    }
    let Some(command) = arguments
        .iter()
        .find_map(|arg| arg.strip_prefix("--huali-runtime-smoke="))
    else {
        return false;
    };
    let runtime = app.state::<DesktopRuntime>();
    match command {
        "lock" | "unlock" => {
            runtime
                .smoke_session
                .store(i8::from(command == "unlock"), Ordering::SeqCst);
            session_changed(app, command == "unlock", false);
        }
        "seed" => {
            if let Some(window) = app.get_webview_window("mascot") {
                let _ = window.eval(
                    "localStorage.setItem('huali_ai_todo_input_draft','runtime-recovery-draft')",
                );
            }
        }
        "hang-mascot" => {
            if let Some(window) = app.get_webview_window("mascot") {
                runtime.record("smoke-renderer-hang", "mascot");
                let _ = window.eval("for (;;) {}");
            }
        }
        command if command.starts_with("snapshot-") => {
            let Ok(sequence) = command[9..].parse::<u64>() else {
                return true;
            };
            for label in LABELS {
                if let Some(window) = app.get_webview_window(label) {
                    let _ = window.eval(format!(r#"
                        if (!window.__runtimeSmokePointerInstalled) {{
                            window.__runtimeSmokePointerInstalled = true;
                            window.__runtimeSmokePointerCount = 0;
                            document.addEventListener('pointerdown', () => window.__runtimeSmokePointerCount++, true);
                        }}
                        window.__TAURI_INTERNALS__.invoke('desktop_runtime_smoke_receipt', {{
                            sequence: {sequence},
                            draftPresent: localStorage.getItem('huali_ai_todo_input_draft') === 'runtime-recovery-draft',
                            domPresent: !!document.querySelector('#app > *'),
                            pointerCount: window.__runtimeSmokePointerCount,
                        }}).catch(() => {{}});
                    "#));
                }
            }
        }
        _ => {}
    }
    true
}

#[tauri::command]
pub fn desktop_runtime_smoke_receipt(
    window: tauri::WebviewWindow,
    sequence: u64,
    draft_present: bool,
    dom_present: bool,
    pointer_count: u64,
) -> bool {
    if !smoke_enabled() {
        return false;
    }
    let Some(nonce) = crate::desktop_release_smoke_nonce() else {
        return false;
    };
    // Window getters must precede locks: they may marshal to the UI thread.
    let visible = window.is_visible().unwrap_or(false);
    let focused = window.is_focused().unwrap_or(false);
    let position = window.outer_position().ok();
    let size = window.outer_size().ok();
    let runtime = window.state::<DesktopRuntime>();
    let entry = {
        let h = runtime.health.lock().unwrap();
        let Some(view) = h.views.get(window.label()) else {
            return false;
        };
        serde_json::json!({
            "sequence": sequence, "draftPresent": draft_present,
            "domPresent": dom_present, "pointerCount": pointer_count,
            "processId": std::process::id(), "epoch": h.epoch,
            "interactive": h.interactive, "repairs": view.repairs,
            "generation": view.native_generation, "mounted": view.application_ready,
            "recovered": view.recovered, "nativeVisible": visible,
            "focused": focused,
            "position": position, "size": size,
        })
    };
    static RECEIPT_LOCK: Mutex<()> = Mutex::new(());
    let Ok(_guard) = RECEIPT_LOCK.lock() else {
        return false;
    };
    let path = std::env::temp_dir().join(format!("huali-runtime-smoke-{nonce}.json"));
    let mut receipt = std::fs::read(&path)
        .ok()
        .and_then(|bytes| {
            serde_json::from_slice::<BTreeMap<String, serde_json::Value>>(&bytes).ok()
        })
        .unwrap_or_default();
    receipt.insert(window.label().into(), entry);
    serde_json::to_vec(&receipt)
        .ok()
        .is_some_and(|bytes| std::fs::write(path, bytes).is_ok())
}
