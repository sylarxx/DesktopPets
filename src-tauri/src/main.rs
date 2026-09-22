#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg_attr(not(windows), allow(dead_code))]
mod runtime_health;
#[cfg_attr(not(windows), allow(dead_code))]
mod runtime_recovery;
#[cfg(windows)]
mod windows_session;

use std::collections::VecDeque;
use std::fs;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::path::PathBuf;
#[cfg(any(target_os = "macos", windows))]
use std::process::Command;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex, OnceLock,
};
use std::thread;
use std::time::{Duration, Instant};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::window::Color;
use tauri::{
    Emitter, LogicalPosition, LogicalSize, Manager, PhysicalPosition, PhysicalSize, Position, Size,
};

// Keep a transparent safety gutter around the sprite. WebView2 can otherwise
// clip the last physical pixel at 125%/150% Windows display scaling.
const MASCOT_WIDTH: f64 = 120.0;
const MASCOT_HEIGHT: f64 = 104.0;
const MASCOT_AVATAR_WIDTH: f64 = 96.0;
const MASCOT_AVATAR_HEIGHT: f64 = 88.0;
const MASCOT_NOTIFICATION_BOTTOM_PADDING: f64 = 8.0;
// Keep the same amount of the safety window visible when peeking from either
// desktop edge. This leaves a discoverable part of Xiaoli on screen.
const MASCOT_PEEK_VISIBLE_WIDTH: f64 = 84.0;
const MASCOT_PEEK_ANIMATION_DURATION_MS: u64 = 560;
const MASCOT_REVEAL_ANIMATION_DURATION_MS: u64 = 480;
const MASCOT_DOCK_ANIMATION_FRAME_MS: u64 = 12;
#[cfg(windows)]
const MASCOT_PEEK_HOVER_POLL_INTERVAL_MS: u64 = 40;
// WebView2 can report a fractional logical position after a DPI-aware resize.
// Treat that sub-pixel drift as resize noise instead of a user drag.
#[cfg(any(not(windows), test))]
const MASCOT_NOTIFICATION_DRAG_EPSILON: f64 = 1.0;
const MASCOT_CONTEXT_MENU_WIDTH: f64 = 216.0;
const MASCOT_CONTEXT_MENU_HEIGHT: f64 = 76.0;
const MASCOT_CONTEXT_MENU_GAP: f64 = 18.0;
const MASCOT_CONTEXT_MENU_ABOVE_VISIBLE_BOTTOM: f64 = 55.0;
const MASCOT_CONTEXT_MENU_BELOW_VISIBLE_TOP: f64 = 9.0;
const MASCOT_CONTEXT_MENU_NAV_LEFT: f64 = 12.0;
const MASCOT_CONTEXT_MENU_TAIL_MIN: f64 = 18.0;
const MASCOT_CONTEXT_MENU_TAIL_MAX: f64 = 174.0;
const MASCOT_CONTEXT_MENU_LAYOUT_ACK_TIMEOUT_MS: u64 = 1200;
#[cfg(windows)]
const MASCOT_COLLAPSE_RECOVERY_TIMEOUT_MS: u64 = 1500;
const MASCOT_SYSTEM_NOTIFICATION_WIDTH: f64 = 320.0;
const MASCOT_SYSTEM_NOTIFICATION_HEIGHT: f64 = 320.0;
const MASCOT_AUTH_NOTIFICATION_HEIGHT: f64 = 192.0;
const MASCOT_SYSTEM_NOTIFICATION_GAP: f64 = 8.0;
const MASCOT_SYSTEM_NOTIFICATION_MARGIN: f64 = 8.0;
const DESKTOP_AUTH_CALLBACK_PREFIX: &str = "huali-ai-mascot://auth-callback";
const DESKTOP_AUTH_CALLBACK_FILE: &str = "huali-ai-mascot-auth-callback.tmp";
const DESKTOP_AUTH_CALLBACK_QUEUE_CAPACITY: usize = 8;
const DESKTOP_DIAGNOSTIC_LOG_FILE: &str = "desktop-diagnostic.jsonl";
const DESKTOP_RELEASE_SMOKE_ENABLED_ENV: &str = "HUALI_AI_RELEASE_SMOKE";
const DESKTOP_RELEASE_SMOKE_AUTH_STATE_ENV: &str = "HUALI_AI_RELEASE_SMOKE_AUTH_STATE";
const DESKTOP_RELEASE_SMOKE_NONCE_ENV: &str = "HUALI_AI_RELEASE_SMOKE_NONCE";
const PANEL_VISIBILITY_EVENT: &str = "huali:panel-visibility";
const MASCOT_CONTEXT_MENU_VISIBILITY_EVENT: &str = "mascot-context-menu-visibility";
const MASCOT_SYSTEM_NOTIFICATION_READY_EVENT: &str = "mascot-system-notification-ready";
const MASCOT_NATIVE_REVEALED_EVENT: &str = "mascot-native-revealed";
#[cfg(windows)]
const MASCOT_NATIVE_HOVER_REVEALED_EVENT: &str = "mascot-native-hover-revealed";
const VISUAL_SMOKE_PEEK_ARGUMENT: &str = "--huali-visual-smoke-peek";

static DESKTOP_AUTH_SMOKE_RECEIPT_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Clone, Default)]
struct PendingDesktopAuthCallback(Arc<Mutex<VecDeque<NativeDesktopAuthCallback>>>);

#[derive(Clone, Default)]
struct MascotDockMotion(Arc<AtomicU64>);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct MascotSystemNotificationStatus {
    ready: bool,
    desired_visible: bool,
    visible: bool,
    desired_compact: bool,
    visible_compact: bool,
    generation: u64,
    client_generation: u64,
}

#[derive(Clone, Default)]
struct MascotSystemNotificationState {
    status: Arc<Mutex<MascotSystemNotificationStatus>>,
    // Native show/hide operations are serialized independently from logical
    // intent. A newer request can invalidate an in-flight generation before it
    // reaches the physical HWND transition.
    transition: Arc<Mutex<()>>,
}

impl MascotSystemNotificationState {
    fn mark_ready(&self) -> Result<bool, String> {
        let mut status = self
            .status
            .lock()
            .map_err(|_| "mascot system notification state is unavailable".to_string())?;
        let first_ready = !status.ready;
        status.ready = true;
        Ok(first_ready)
    }

    fn is_ready(&self) -> bool {
        self.status
            .lock()
            .map(|status| status.ready)
            .unwrap_or(false)
    }

    fn request_show(
        &self,
        compact: bool,
        client_generation: Option<u64>,
    ) -> Result<Option<u64>, String> {
        let mut status = self
            .status
            .lock()
            .map_err(|_| "mascot system notification state is unavailable".to_string())?;
        if !status.ready {
            return Ok(None);
        }
        if let Some(client_generation) = client_generation {
            if client_generation <= status.client_generation {
                return Ok(None);
            }
            status.client_generation = client_generation;
        }
        status.generation = status.generation.wrapping_add(1);
        status.desired_visible = true;
        status.desired_compact = compact;
        Ok(Some(status.generation))
    }

    fn request_hide(&self, client_generation: Option<u64>) -> Result<Option<u64>, String> {
        let mut status = self
            .status
            .lock()
            .map_err(|_| "mascot system notification state is unavailable".to_string())?;
        if let Some(client_generation) = client_generation {
            if client_generation <= status.client_generation {
                return Ok(None);
            }
            status.client_generation = client_generation;
        }
        status.generation = status.generation.wrapping_add(1);
        status.desired_visible = false;
        Ok(Some(status.generation))
    }

    fn can_show(&self, generation: u64, compact: bool) -> bool {
        self.status
            .lock()
            .map(|status| {
                status.ready
                    && status.desired_visible
                    && status.generation == generation
                    && status.desired_compact == compact
            })
            .unwrap_or(false)
    }

    fn can_hide(&self, generation: u64) -> bool {
        self.status
            .lock()
            .map(|status| !status.desired_visible && status.generation == generation)
            .unwrap_or(false)
    }

    fn mark_visible(&self, generation: u64, compact: bool) -> bool {
        let Ok(mut status) = self.status.lock() else {
            return false;
        };
        if !status.ready
            || !status.desired_visible
            || status.generation != generation
            || status.desired_compact != compact
        {
            return false;
        }
        status.visible = true;
        status.visible_compact = compact;
        true
    }

    fn cancel_show(&self, generation: u64) {
        let Ok(mut status) = self.status.lock() else {
            return;
        };
        if status.generation != generation {
            return;
        }
        status.desired_visible = false;
        status.visible = false;
    }

    fn mark_physical_hidden(&self) {
        if let Ok(mut status) = self.status.lock() {
            status.visible = false;
        }
    }

    fn visible_compact(&self) -> Option<bool> {
        self.status
            .lock()
            .ok()
            .and_then(|status| status.visible.then_some(status.visible_compact))
    }
}

#[derive(Clone)]
struct InitialMascotPlacement(Arc<AtomicBool>);

impl Default for InitialMascotPlacement {
    fn default() -> Self {
        Self(Arc::new(AtomicBool::new(true)))
    }
}

impl MascotDockMotion {
    fn cancel(&self) -> u64 {
        self.0.fetch_add(1, Ordering::SeqCst) + 1
    }
}

#[cfg(not(windows))]
#[derive(Clone, Copy)]
struct MascotNotificationLayout {
    restore_position: LogicalPosition<f64>,
    expanded_position: LogicalPosition<f64>,
}

#[cfg(not(windows))]
#[derive(Clone, Default)]
struct MascotNotificationLayoutState(Arc<Mutex<Option<MascotNotificationLayout>>>);

#[cfg(windows)]
#[derive(Clone, Copy)]
struct StagedMascotPosition {
    generation: u64,
    position: PhysicalPosition<i32>,
}

#[cfg(windows)]
#[derive(Clone, Default)]
struct MascotNotificationLayoutState {
    staged: Arc<Mutex<Option<StagedMascotPosition>>>,
    generation: Arc<AtomicU64>,
}

#[cfg(windows)]
impl MascotNotificationLayoutState {
    fn stage_collapsed_position(&self, position: PhysicalPosition<i32>) -> Result<u64, String> {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let mut staged = self
            .staged
            .lock()
            .map_err(|_| "mascot notification layout state is unavailable".to_string())?;
        *staged = Some(StagedMascotPosition {
            generation,
            position,
        });
        Ok(generation)
    }

    fn restore_staged_position(&self, window: &tauri::WebviewWindow) -> Result<bool, String> {
        let mut current = self
            .staged
            .lock()
            .map_err(|_| "mascot notification layout state is unavailable".to_string())?;
        let Some(staged) = *current else {
            return Ok(false);
        };
        window
            .set_position(Position::Physical(staged.position))
            .map_err(|error| format!("failed to restore staged mascot position: {error}"))?;
        *current = None;
        Ok(true)
    }

    fn restore_staged_position_for_generation(
        &self,
        window: &tauri::WebviewWindow,
        generation: u64,
    ) -> Result<bool, String> {
        let mut current = self
            .staged
            .lock()
            .map_err(|_| "mascot notification layout state is unavailable".to_string())?;
        let Some(staged) = *current else {
            return Ok(false);
        };
        if staged.generation != generation {
            return Ok(false);
        }
        window
            .set_position(Position::Physical(staged.position))
            .map_err(|error| format!("failed to recover staged mascot position: {error}"))?;
        *current = None;
        Ok(true)
    }
}

fn mascot_notification_layout_state() -> MascotNotificationLayoutState {
    #[cfg(windows)]
    {
        MascotNotificationLayoutState::default()
    }

    #[cfg(not(windows))]
    {
        MascotNotificationLayoutState::default()
    }
}

fn restore_staged_mascot_position(app: &tauri::AppHandle, window: &tauri::WebviewWindow) {
    #[cfg(windows)]
    {
        let state = app.state::<MascotNotificationLayoutState>();
        let _ = state.restore_staged_position(window);
    }

    #[cfg(not(windows))]
    let _ = (app, window);
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct MascotContextMenuStatus {
    ready: bool,
    desired_visible: bool,
    visible: bool,
    awaiting_layout_ack: bool,
    generation: u64,
}

#[derive(Clone, Default)]
struct MascotContextMenuState {
    status: Arc<Mutex<MascotContextMenuStatus>>,
    // Serializes native show/hide operations. The generation guards logical
    // intent; this lock prevents a stale show from physically overtaking a
    // newer hide while WebView2 is processing window messages.
    transition: Arc<Mutex<()>>,
}

impl MascotContextMenuState {
    fn request_show(&self) -> Result<u64, String> {
        let mut status = self
            .status
            .lock()
            .map_err(|_| "mascot context menu state is unavailable".to_string())?;
        status.generation = status.generation.wrapping_add(1);
        status.desired_visible = true;
        status.awaiting_layout_ack = false;
        // A new show request supersedes the focus state of any older menu
        // generation. Its delayed Focused(false) event must not cancel this
        // newer request while the independent window is being repositioned.
        status.visible = false;
        Ok(status.generation)
    }

    fn request_hide(&self) -> Result<u64, String> {
        let mut status = self
            .status
            .lock()
            .map_err(|_| "mascot context menu state is unavailable".to_string())?;
        status.generation = status.generation.wrapping_add(1);
        status.desired_visible = false;
        status.visible = false;
        status.awaiting_layout_ack = false;
        Ok(status.generation)
    }

    fn mark_ready(&self) -> Result<Option<u64>, String> {
        let mut status = self
            .status
            .lock()
            .map_err(|_| "mascot context menu state is unavailable".to_string())?;
        status.ready = true;
        Ok(status.desired_visible.then_some(status.generation))
    }

    fn can_prepare(&self, generation: u64) -> bool {
        self.status
            .lock()
            .map(|status| status.ready && status.desired_visible && status.generation == generation)
            .unwrap_or(false)
    }

    #[cfg(windows)]
    fn is_ready(&self) -> bool {
        self.status
            .lock()
            .map(|status| status.ready)
            .unwrap_or(false)
    }

    fn mark_awaiting_layout_ack(&self, generation: u64) -> bool {
        let Ok(mut status) = self.status.lock() else {
            return false;
        };
        if !status.ready || !status.desired_visible || status.generation != generation {
            return false;
        }
        status.awaiting_layout_ack = true;
        true
    }

    fn can_ack_layout(&self, generation: u64) -> bool {
        self.status
            .lock()
            .map(|status| {
                status.ready
                    && status.desired_visible
                    && status.awaiting_layout_ack
                    && status.generation == generation
            })
            .unwrap_or(false)
    }

    fn mark_visible(&self, generation: u64) -> bool {
        let Ok(mut status) = self.status.lock() else {
            return false;
        };
        if !status.ready
            || !status.desired_visible
            || !status.awaiting_layout_ack
            || status.generation != generation
        {
            return false;
        }
        status.awaiting_layout_ack = false;
        status.visible = true;
        true
    }

    fn cancel_generation(&self, generation: u64) -> bool {
        let Ok(mut status) = self.status.lock() else {
            return false;
        };
        if status.generation != generation {
            return false;
        }
        status.generation = status.generation.wrapping_add(1);
        status.desired_visible = false;
        status.visible = false;
        status.awaiting_layout_ack = false;
        true
    }

    fn expire_pending_show(&self, generation: u64) -> bool {
        let should_expire = self
            .status
            .lock()
            .map(|status| {
                status.generation == generation && status.desired_visible && !status.visible
            })
            .unwrap_or(false);
        should_expire && self.cancel_generation(generation)
    }

    fn is_visible(&self) -> bool {
        self.status
            .lock()
            .map(|status| status.visible)
            .unwrap_or(false)
    }

    #[cfg(test)]
    fn snapshot(&self) -> MascotContextMenuStatus {
        self.status.lock().map(|status| *status).unwrap_or_default()
    }
}

#[derive(Clone, Default)]
struct MascotDragMonitor(Arc<AtomicU64>);

#[derive(Clone, Default)]
struct MascotPeekHoverMonitor(Arc<AtomicU64>);

#[derive(Clone, Copy, Default)]
struct PanelActivity {
    has_text: bool,
    focused: bool,
}

#[derive(Clone, Default)]
struct PanelActivityState(Arc<Mutex<PanelActivity>>);

#[derive(Clone)]
struct PanelLayoutState(Arc<Mutex<f64>>);

impl Default for PanelLayoutState {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(PANEL_COMPACT_HEIGHT)))
    }
}

impl PanelLayoutState {
    fn set_height(&self, height: f64) {
        if let Ok(mut requested_height) = self.0.lock() {
            *requested_height = height;
        }
    }

    fn height(&self) -> f64 {
        self.0
            .lock()
            .map(|height| *height)
            .unwrap_or(PANEL_COMPACT_HEIGHT)
    }
}

impl PanelActivityState {
    fn set(&self, has_text: bool, focused: bool) {
        if let Ok(mut activity) = self.0.lock() {
            *activity = PanelActivity { has_text, focused };
        }
    }

    fn has_content(&self) -> bool {
        self.0
            .lock()
            .map(|activity| activity.has_text)
            .unwrap_or(true)
    }

    fn is_engaged(&self) -> bool {
        self.0
            .lock()
            .map(|activity| activity.has_text || activity.focused)
            .unwrap_or(false)
    }
}

impl MascotDragMonitor {
    fn start(&self) -> u64 {
        self.0.fetch_add(1, Ordering::SeqCst) + 1
    }
}

impl MascotPeekHoverMonitor {
    fn start(&self) -> u64 {
        self.0.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn cancel(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[cfg(any(windows, test))]
fn async_key_state_is_pressed(state: i16) -> bool {
    state as u16 & 0x8000 != 0
}

#[cfg(windows)]
fn monitor_native_drag(app: tauri::AppHandle, monitor: MascotDragMonitor, token: u64) {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};

    thread::spawn(move || {
        let mut last_mascot_position = None;
        loop {
            if monitor.0.load(Ordering::SeqCst) != token {
                return;
            }

            // Follow at the same cadence as the native dock animation and only
            // when Windows has actually moved the mascot HWND. This keeps one
            // compositor-paced path and avoids stacking duplicate position
            // commands from both polling and WindowEvent::Moved.
            let mascot_position = app
                .get_webview_window("mascot")
                .and_then(|window| window.outer_position().ok());
            if mascot_position != last_mascot_position {
                sync_visible_panel_to_mascot(&app);
                sync_visible_mascot_system_notification_to_mascot(&app);
                last_mascot_position = mascot_position;
            }

            let button_state = unsafe { GetAsyncKeyState(VK_LBUTTON as i32) };
            if !async_key_state_is_pressed(button_state) {
                // Mouse-up gets one exact final anchor even if it lands between
                // two monitor frames.
                sync_visible_panel_to_mascot(&app);
                sync_visible_mascot_system_notification_to_mascot(&app);
                let _ = app.emit_to("mascot", "mascot-native-drag-ended", ());
                return;
            }

            thread::sleep(Duration::from_millis(MASCOT_DOCK_ANIMATION_FRAME_MS));
        }
    });
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct NativeDesktopAuthCallback {
    callback_url: Option<String>,
    argument_count: usize,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopReleaseSmokeConfig {
    auth_state: String,
}

fn desktop_diagnostic_log_path(app: &tauri::AppHandle) -> PathBuf {
    app.path()
        .app_log_dir()
        .unwrap_or_else(|_| std::env::temp_dir().join("huali-ai-desktop-logs"))
        .join(DESKTOP_DIAGNOSTIC_LOG_FILE)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct DesktopDiagnosticCleanupResult {
    primary_existed: bool,
    primary_removed: bool,
    rotated_existed: bool,
    rotated_removed: bool,
}

fn remove_desktop_diagnostic_file_if_present(path: &Path) -> (bool, bool) {
    let existed = path.is_file();
    let removed = existed && fs::remove_file(path).is_ok();
    (existed, removed)
}

fn cleanup_desktop_diagnostic_files(primary_path: &Path) -> DesktopDiagnosticCleanupResult {
    let rotated_path = primary_path.with_extension("jsonl.1");
    let (primary_existed, primary_removed) =
        remove_desktop_diagnostic_file_if_present(primary_path);
    let (rotated_existed, rotated_removed) =
        remove_desktop_diagnostic_file_if_present(&rotated_path);
    DesktopDiagnosticCleanupResult {
        primary_existed,
        primary_removed,
        rotated_existed,
        rotated_removed,
    }
}

fn cleanup_desktop_diagnostic_logs(app: &tauri::AppHandle) -> DesktopDiagnosticCleanupResult {
    cleanup_desktop_diagnostic_files(&desktop_diagnostic_log_path(app))
}

fn write_desktop_diagnostic_event(
    _app: &tauri::AppHandle,
    _event: &str,
    _fields: serde_json::Map<String, serde_json::Value>,
) -> bool {
    false
}

fn diagnostic_fields(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    value.as_object().cloned().unwrap_or_default()
}

#[tauri::command]
fn record_desktop_diagnostic_event(
    app: tauri::AppHandle,
    event: String,
    fields: serde_json::Map<String, serde_json::Value>,
) -> bool {
    write_desktop_diagnostic_event(&app, &event, fields)
}

#[tauri::command]
fn get_desktop_diagnostic_log_path(app: tauri::AppHandle) -> String {
    desktop_diagnostic_log_path(&app)
        .to_string_lossy()
        .into_owned()
}

fn valid_release_smoke_value(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
}

fn desktop_release_smoke_nonce() -> Option<String> {
    if !matches!(
        std::env::var(DESKTOP_RELEASE_SMOKE_ENABLED_ENV).as_deref(),
        Ok("1")
    ) {
        return None;
    }
    std::env::var(DESKTOP_RELEASE_SMOKE_NONCE_ENV)
        .ok()
        .filter(|value| valid_release_smoke_value(value))
}

#[tauri::command]
fn get_desktop_release_smoke_config() -> Option<DesktopReleaseSmokeConfig> {
    desktop_release_smoke_nonce()?;
    let auth_state = std::env::var(DESKTOP_RELEASE_SMOKE_AUTH_STATE_ENV).ok()?;
    valid_release_smoke_value(&auth_state).then_some(DesktopReleaseSmokeConfig { auth_state })
}

fn persist_desktop_release_smoke_fields(fields: serde_json::Value) -> bool {
    let Some(nonce) = desktop_release_smoke_nonce() else {
        return false;
    };
    let Ok(_receipt_guard) = DESKTOP_AUTH_SMOKE_RECEIPT_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
    else {
        return false;
    };
    let path = std::env::temp_dir().join(format!("huali-ai-desktop-auth-smoke-{nonce}.json"));
    let mut receipt = fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    if let Some(fields) = fields.as_object() {
        receipt.extend(fields.clone());
    }
    serde_json::to_vec(&receipt)
        .ok()
        .is_some_and(|serialized| fs::write(path, serialized).is_ok())
}

#[tauri::command]
fn record_desktop_release_smoke_session(
    session_committed: bool,
    subscriptions_started: bool,
    reminder_types_queued: u64,
) -> bool {
    persist_desktop_release_smoke_fields(serde_json::json!({
        "sessionCommitted": session_committed,
        "subscriptionsStarted": subscriptions_started,
        "reminderTypesQueued": reminder_types_queued,
    }))
}

#[tauri::command]
fn record_desktop_release_smoke_restart(session_restored: bool) -> bool {
    persist_desktop_release_smoke_fields(serde_json::json!({
        "sessionRestoredAfterRestart": session_restored,
    }))
}

fn find_desktop_auth_callback(args: &[String]) -> Option<String> {
    args.iter()
        .find_map(|arg| normalize_desktop_auth_callback_argument(arg))
}

fn normalize_desktop_auth_callback_argument(argument: &str) -> Option<String> {
    let normalized = argument.trim().trim_matches('"').trim();
    let prefix = normalized.get(..DESKTOP_AUTH_CALLBACK_PREFIX.len())?;
    prefix
        .eq_ignore_ascii_case(DESKTOP_AUTH_CALLBACK_PREFIX)
        .then(|| normalized.to_owned())
}

fn desktop_auth_callback_query_value<'a>(callback_url: &'a str, name: &str) -> Option<&'a str> {
    let (_, query_and_fragment) = callback_url.split_once('?')?;
    let query = query_and_fragment.split('#').next().unwrap_or_default();
    query.split('&').find_map(|pair| {
        pair.split_once('=')
            .and_then(|(pair_name, pair_value)| (pair_name == name).then_some(pair_value))
    })
}

fn desktop_auth_callback_has_value(callback_url: &str, name: &str) -> bool {
    desktop_auth_callback_query_value(callback_url, name).is_some_and(|value| !value.is_empty())
}

fn desktop_auth_callback_diagnostic_fields(
    callback_url: &str,
) -> serde_json::Map<String, serde_json::Value> {
    let callback_without_fragment = callback_url.split('#').next().unwrap_or_default();
    let callback_before_query = callback_without_fragment
        .split('?')
        .next()
        .unwrap_or_default();
    let raw_query = callback_without_fragment
        .split_once('?')
        .map(|(_, query)| query)
        .unwrap_or_default();
    let fragment = callback_url
        .split_once('#')
        .map(|(_, fragment)| fragment)
        .unwrap_or_default();
    let prefix_matches = callback_url
        .get(..DESKTOP_AUTH_CALLBACK_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(DESKTOP_AUTH_CALLBACK_PREFIX));
    let callback_prefix_boundary = callback_url
        .get(DESKTOP_AUTH_CALLBACK_PREFIX.len()..)
        .and_then(|suffix| suffix.chars().next())
        .map(|character| character.to_string())
        .unwrap_or_default();
    let callback_prefix_remainder_before_query = callback_before_query
        .get(DESKTOP_AUTH_CALLBACK_PREFIX.len()..)
        .unwrap_or_default();
    let callback_url_utf8_hex = callback_url
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();

    diagnostic_fields(serde_json::json!({
        "callbackUrl": callback_url,
        "callbackUrlLength": callback_url.chars().count(),
        "callbackUrlUtf8Hex": callback_url_utf8_hex,
        "callbackBeforeQuery": callback_before_query,
        "rawQuery": raw_query,
        "fragment": fragment,
        "expectedCallbackPrefix": DESKTOP_AUTH_CALLBACK_PREFIX,
        "callbackPrefixMatches": prefix_matches,
        "callbackPrefixBoundary": callback_prefix_boundary,
        "callbackPrefixRemainderBeforeQuery": callback_prefix_remainder_before_query,
        "state": desktop_auth_callback_query_value(callback_url, "state"),
        "token": desktop_auth_callback_query_value(callback_url, "token"),
        "userId": desktop_auth_callback_query_value(callback_url, "userId"),
        "userName": desktop_auth_callback_query_value(callback_url, "userName"),
        "department": desktop_auth_callback_query_value(callback_url, "department"),
        "hasState": desktop_auth_callback_has_value(callback_url, "state"),
        "hasToken": desktop_auth_callback_has_value(callback_url, "token"),
        "hasUserId": desktop_auth_callback_has_value(callback_url, "userId"),
    }))
}

fn persist_desktop_auth_smoke_receipt(
    callback_url: &str,
    forwarded_to_running_instance: Option<bool>,
    renderer_outcome: Option<&str>,
) -> bool {
    let Some(nonce) = desktop_auth_callback_query_value(callback_url, "smokeNonce") else {
        return false;
    };
    if nonce.is_empty()
        || nonce.len() > 64
        || !nonce
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
    {
        return false;
    }

    let Ok(_receipt_guard) = DESKTOP_AUTH_SMOKE_RECEIPT_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
    else {
        return false;
    };

    // The release gate records only field presence, never the callback token or
    // user identity. Restricting the path to the OS temp directory also avoids
    // turning the diagnostic into an arbitrary production file writer.
    let path = std::env::temp_dir().join(format!("huali-ai-desktop-auth-smoke-{nonce}.json"));
    let mut receipt = fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    receipt.insert("callbackReceived".to_owned(), serde_json::Value::Bool(true));
    receipt.insert(
        "hasState".to_owned(),
        serde_json::Value::Bool(desktop_auth_callback_has_value(callback_url, "state")),
    );
    receipt.insert(
        "hasToken".to_owned(),
        serde_json::Value::Bool(desktop_auth_callback_has_value(callback_url, "token")),
    );
    receipt.insert(
        "hasUserId".to_owned(),
        serde_json::Value::Bool(desktop_auth_callback_has_value(callback_url, "userId")),
    );
    if forwarded_to_running_instance == Some(true) {
        receipt.insert(
            "forwardedToRunningInstance".to_owned(),
            serde_json::Value::Bool(true),
        );
    }
    if let Some(renderer_outcome) = renderer_outcome {
        receipt.insert("rendererReceived".to_owned(), serde_json::Value::Bool(true));
        receipt.insert(
            "rendererOutcome".to_owned(),
            serde_json::Value::String(renderer_outcome.to_owned()),
        );
    }
    serde_json::to_vec(&receipt)
        .ok()
        .is_some_and(|serialized| fs::write(path, serialized).is_ok())
}

fn desktop_auth_callback_file() -> PathBuf {
    std::env::temp_dir().join(DESKTOP_AUTH_CALLBACK_FILE)
}

fn persist_startup_desktop_auth_callback(callback_url: &str) {
    let _ = fs::write(desktop_auth_callback_file(), callback_url.as_bytes());
}

fn take_persisted_desktop_auth_callback() -> Option<NativeDesktopAuthCallback> {
    let path = desktop_auth_callback_file();
    let callback_url = fs::read_to_string(&path).ok();
    let _ = fs::remove_file(path);

    callback_url.map(|callback_url| NativeDesktopAuthCallback {
        callback_url: Some(callback_url),
        argument_count: 2,
    })
}

impl PendingDesktopAuthCallback {
    fn capture(&self, args: &[String]) -> Option<String> {
        let callback_url = find_desktop_auth_callback(args);
        if let Some(callback_url) = callback_url.as_ref() {
            if let Ok(mut pending) = self.0.lock() {
                let duplicate = pending
                    .iter()
                    .any(|item| item.callback_url.as_deref() == Some(callback_url.as_str()));
                if !duplicate {
                    if pending.len() >= DESKTOP_AUTH_CALLBACK_QUEUE_CAPACITY {
                        pending.pop_front();
                    }
                    pending.push_back(NativeDesktopAuthCallback {
                        callback_url: Some(callback_url.clone()),
                        argument_count: args.len(),
                    });
                }
            }
        }
        callback_url
    }

    fn take(&self) -> Option<NativeDesktopAuthCallback> {
        self.0.lock().ok()?.pop_front()
    }
}

#[tauri::command]
fn record_desktop_auth_renderer_receipt(
    app: tauri::AppHandle,
    callback_url: String,
    outcome: String,
) -> bool {
    if normalize_desktop_auth_callback_argument(&callback_url).is_none()
        || !matches!(
            outcome.as_str(),
            "success" | "error:expired" | "error:missing-identity"
        )
    {
        return false;
    }

    write_desktop_diagnostic_event(&app, "auth.callback.renderer_outcome", {
        let mut fields = desktop_auth_callback_diagnostic_fields(&callback_url);
        fields.extend(diagnostic_fields(serde_json::json!({
            "outcome": outcome,
        })));
        fields
    });
    persist_desktop_auth_smoke_receipt(&callback_url, None, Some(&outcome))
}

#[tauri::command]
fn take_desktop_auth_callback(
    app: tauri::AppHandle,
    state: tauri::State<'_, PendingDesktopAuthCallback>,
) -> Option<NativeDesktopAuthCallback> {
    let callback = state.take().or_else(take_persisted_desktop_auth_callback);
    if let Some(callback) = callback.as_ref() {
        let mut fields = diagnostic_fields(serde_json::json!({
            "argumentCount": callback.argument_count,
            "callbackPresent": callback.callback_url.is_some(),
        }));
        if let Some(callback_url) = callback.callback_url.as_deref() {
            fields.extend(desktop_auth_callback_diagnostic_fields(callback_url));
        }
        write_desktop_diagnostic_event(&app, "auth.callback.native_dequeued", fields);
    }
    callback
}
const MASCOT_NOTIFICATION_WIDTH: f64 = 320.0;
const MASCOT_NOTIFICATION_HEIGHT: f64 = 480.0;
// Compact overlays need enough transparent safety space for the context menu,
// its tail and shadow at every Windows DPI. 240x224 keeps a deliberate visual
// gap above the mascot while preserving the same 8px bottom safety gutter as
// the collapsed and expanded layouts.
const MASCOT_MESSAGE_WIDTH: f64 = 240.0;
const MASCOT_MESSAGE_HEIGHT: f64 = 176.0;
const PANEL_WIDTH: f64 = 380.0;
const PANEL_COMPACT_HEIGHT: f64 = 78.0;
const PANEL_MAX_HEIGHT: f64 = 380.0;
const SCREEN_MARGIN: f64 = 24.0;
const MASCOT_REST_RIGHT_MARGIN: f64 = 30.0;
const MASCOT_REST_BOTTOM_MARGIN: f64 = 38.0;
#[cfg(not(windows))]
const PANEL_GAP: f64 = 8.0;
const TRANSPARENT: Option<Color> = Some(Color(0, 0, 0, 0));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MascotDockSide {
    Left,
    Right,
}

impl MascotDockSide {
    fn as_str(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
        }
    }
}

fn harden_transparent_window(window: &tauri::WebviewWindow) {
    let _ = window.set_shadow(false);
    let _ = window.set_background_color(TRANSPARENT);
}

fn show_window_without_activation(window: &tauri::WebviewWindow) -> Result<(), String> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SetWindowPos, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER,
            SWP_SHOWWINDOW,
        };

        let hwnd = window
            .hwnd()
            .map_err(|error| format!("failed to access non-activating window HWND: {error}"))?;
        let shown = unsafe {
            SetWindowPos(
                hwnd.0,
                std::ptr::null_mut(),
                0,
                0,
                0,
                0,
                SWP_NOACTIVATE
                    | SWP_NOMOVE
                    | SWP_NOOWNERZORDER
                    | SWP_NOSIZE
                    | SWP_NOZORDER
                    | SWP_SHOWWINDOW,
            )
        };
        if shown == 0 {
            Err("failed to show native window without activation".to_string())
        } else {
            Ok(())
        }
    }

    #[cfg(not(windows))]
    window
        .show()
        .map_err(|error| format!("failed to show window: {error}"))
}

fn hide_transparent_window_safely(window: &tauri::WebviewWindow) -> bool {
    // Visibility alone is not a sufficient safety boundary for a transparent,
    // always-on-top WebView2 HWND: a delayed or failed hide can leave an
    // invisible rectangle in the desktop hit-test path. Queue click-through
    // first so the fallback state can never block another application.
    let click_through = window.set_ignore_cursor_events(true).is_ok();
    let hidden = window.hide().is_ok();
    click_through && hidden
}

fn show_interactive_window(window: &tauri::WebviewWindow, activate: bool) -> bool {
    if !window
        .state::<runtime_recovery::DesktopRuntime>()
        .interactive()
    {
        return false;
    }
    if window.set_ignore_cursor_events(false).is_err() {
        let _ = hide_transparent_window_safely(window);
        return false;
    }

    let shown = if activate {
        window.show().map_err(|error| error.to_string())
    } else {
        show_window_without_activation(window)
    };
    if shown.is_err() {
        let _ = hide_transparent_window_safely(window);
        return false;
    }

    if activate && window.set_focus().is_err() {
        let _ = hide_transparent_window_safely(window);
        return false;
    }
    true
}

#[cfg(windows)]
fn schedule_mascot_interactivity_recovery(
    window: tauri::WebviewWindow,
    interaction_generation: MascotPeekHoverMonitor,
    token: u64,
) {
    thread::spawn(move || {
        for delay in [120_u64, 300_u64] {
            thread::sleep(Duration::from_millis(delay));
            if interaction_generation.0.load(Ordering::SeqCst) != token {
                return;
            }
            if !matches!(window.is_visible(), Ok(true)) {
                return;
            }
            // On a cold WebView2 start, the hosted child HWND can finish its
            // style transition after the Tauri show call. Reassert hit testing
            // while the same mascot HWND is still visible; a user hide cancels
            // this recovery naturally through the visibility check above.
            let _ = window.set_ignore_cursor_events(false);
        }
    });
}

#[cfg(not(windows))]
fn schedule_mascot_interactivity_recovery(
    _window: tauri::WebviewWindow,
    _interaction_generation: MascotPeekHoverMonitor,
    _token: u64,
) {
}

fn mascot_logical_size(mascot: &tauri::WebviewWindow) -> (f64, f64) {
    let scale = mascot.scale_factor().unwrap_or(1.0);
    mascot
        .outer_size()
        .ok()
        .map(|size| {
            let logical = size.to_logical::<f64>(scale);
            (logical.width, logical.height)
        })
        .unwrap_or((MASCOT_WIDTH, MASCOT_HEIGHT))
}

fn sync_panel_if_visible(app: &tauri::AppHandle) {
    if let (Some(panel), Some(mascot)) = (
        app.get_webview_window("panel"),
        app.get_webview_window("mascot"),
    ) {
        if matches!(panel.is_visible(), Ok(true)) {
            let requested_height = app.state::<PanelLayoutState>().height();
            let _ = place_panel_near_mascot(&panel, &mascot, requested_height);
        }
    }
}

#[cfg(not(windows))]
fn place_bottom_right(window: &tauri::WebviewWindow, width: f64, height: f64) {
    let _ = window.set_size(Size::Logical(LogicalSize { width, height }));

    let monitor = window
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| window.primary_monitor().ok().flatten());
    if let Some(monitor) = monitor {
        let scale = monitor.scale_factor();
        let screen_size = monitor.work_area().size.to_logical::<f64>(scale);
        let screen_pos = monitor.work_area().position.to_logical::<f64>(scale);
        let x = screen_pos.x + screen_size.width - width - SCREEN_MARGIN;
        let y = screen_pos.y + screen_size.height - height - SCREEN_MARGIN;
        let _ = window.set_position(Position::Logical(LogicalPosition { x, y }));
    }
}

fn mascot_bottom_right_position(
    work_pos: LogicalPosition<f64>,
    work_size: LogicalSize<f64>,
) -> LogicalPosition<f64> {
    LogicalPosition {
        x: work_pos.x + work_size.width - MASCOT_WIDTH - MASCOT_REST_RIGHT_MARGIN,
        y: work_pos.y + work_size.height - MASCOT_HEIGHT - MASCOT_REST_BOTTOM_MARGIN,
    }
}

fn place_mascot_bottom_right(window: &tauri::WebviewWindow) -> bool {
    // The configured window already starts at the collapsed mascot size.
    // Do not set it again here: the frontend may have already expanded the
    // window for its first login/message card, and a late startup resize would
    // clip that card down to a single horizontal border above the mascot.
    let Some(monitor) = window
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| window.primary_monitor().ok().flatten())
    else {
        return false;
    };
    let scale = monitor.scale_factor();
    let work_size = monitor.work_area().size.to_logical::<f64>(scale);
    let work_pos = monitor.work_area().position.to_logical::<f64>(scale);
    let collapsed_position = mascot_bottom_right_position(work_pos, work_size);
    let collapsed_offset = mascot_avatar_offset(MASCOT_WIDTH, MASCOT_HEIGHT, false, false);
    let (width, height) = mascot_logical_size(window);
    let visible = width > MASCOT_WIDTH + 1.0 || height > MASCOT_HEIGHT + 1.0;
    let compact =
        visible && width <= MASCOT_MESSAGE_WIDTH + 1.0 && height <= MASCOT_MESSAGE_HEIGHT + 1.0;
    let current_offset = mascot_avatar_offset(width, height, visible, compact);
    let position = clamp_position_to_rect(
        align_window_to_avatar(collapsed_position, collapsed_offset, current_offset),
        width,
        height,
        work_pos,
        work_size,
    );

    window.set_position(Position::Logical(position)).is_ok()
}

fn ensure_initial_mascot_placement(window: &tauri::WebviewWindow, state: &InitialMascotPlacement) {
    if state.0.load(Ordering::SeqCst) && place_mascot_bottom_right(window) {
        state.0.store(false, Ordering::SeqCst);
    }
}

#[cfg(any(not(windows), test))]
fn mascot_dock_target(
    window: &tauri::WebviewWindow,
    width: f64,
    height: f64,
    peek: bool,
) -> Option<LogicalPosition<f64>> {
    let monitor = window.current_monitor().ok().flatten()?;
    let scale = monitor.scale_factor();
    let work_size = monitor.work_area().size.to_logical::<f64>(scale);
    let work_pos = monitor.work_area().position.to_logical::<f64>(scale);
    let current_y = window
        .outer_position()
        .ok()
        .map(|position| position.to_logical::<f64>(scale).y)
        .unwrap_or(work_pos.y + work_size.height - height - SCREEN_MARGIN);
    let current_x = window
        .outer_position()
        .ok()
        .map(|position| position.to_logical::<f64>(scale).x)
        .unwrap_or(work_pos.x + work_size.width - width - MASCOT_REST_RIGHT_MARGIN);
    let min_y = work_pos.y + SCREEN_MARGIN;
    let max_y = work_pos.y + work_size.height - height - MASCOT_REST_BOTTOM_MARGIN;
    let side = nearest_dock_side(current_x, width, work_pos.x, work_size.width);
    let x = mascot_dock_x(side, peek, width, work_pos.x, work_size.width);

    Some(LogicalPosition {
        x,
        y: current_y.clamp(min_y, max_y.max(min_y)),
    })
}

#[cfg(any(not(windows), test))]
fn nearest_dock_side(
    position_x: f64,
    width: f64,
    work_left: f64,
    work_width: f64,
) -> MascotDockSide {
    let window_center = position_x + width / 2.0;
    let work_center = work_left + work_width / 2.0;
    if window_center <= work_center {
        MascotDockSide::Left
    } else {
        MascotDockSide::Right
    }
}

#[cfg(any(not(windows), test))]
fn mascot_dock_x(
    side: MascotDockSide,
    peek: bool,
    width: f64,
    work_left: f64,
    work_width: f64,
) -> f64 {
    match (side, peek) {
        (MascotDockSide::Left, true) => work_left - width + MASCOT_PEEK_VISIBLE_WIDTH,
        (MascotDockSide::Left, false) => work_left + MASCOT_REST_RIGHT_MARGIN,
        (MascotDockSide::Right, true) => work_left + work_width - MASCOT_PEEK_VISIBLE_WIDTH,
        (MascotDockSide::Right, false) => work_left + work_width - width - MASCOT_REST_RIGHT_MARGIN,
    }
}

#[cfg(any(windows, test))]
fn mascot_dock_physical_target(
    current_position: PhysicalPosition<i32>,
    window_size: PhysicalSize<u32>,
    work_area: PhysicalRect,
    scale: f64,
    peek: bool,
) -> (PhysicalPosition<i32>, MascotDockSide) {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let work_left = i64::from(work_area.x);
    let work_top = i64::from(work_area.y);
    let work_right = work_left + i64::from(work_area.width);
    let work_bottom = work_top + i64::from(work_area.height);
    let window_width = i64::from(window_size.width);
    let window_height = i64::from(window_size.height);
    let window_center = i64::from(current_position.x) + window_width / 2;
    let work_center = work_left + i64::from(work_area.width) / 2;
    let side = if window_center <= work_center {
        MascotDockSide::Left
    } else {
        MascotDockSide::Right
    };
    let visible_width = logical_to_physical(MASCOT_PEEK_VISIBLE_WIDTH, scale);
    let rest_margin = logical_to_physical(MASCOT_REST_RIGHT_MARGIN, scale);
    let x = match (side, peek) {
        (MascotDockSide::Left, true) => work_left - window_width + visible_width,
        (MascotDockSide::Left, false) => work_left + rest_margin,
        (MascotDockSide::Right, true) => work_right - visible_width,
        (MascotDockSide::Right, false) => work_right - window_width - rest_margin,
    };
    let min_y = work_top + logical_to_physical(SCREEN_MARGIN, scale);
    let max_y = work_bottom - window_height - logical_to_physical(MASCOT_REST_BOTTOM_MARGIN, scale);
    let y = if max_y >= min_y {
        i64::from(current_position.y).clamp(min_y, max_y)
    } else {
        min_y
    };

    (
        PhysicalPosition {
            x: x.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
            y: y.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        },
        side,
    )
}

#[cfg(any(not(windows), test))]
fn current_mascot_dock_side(window: &tauri::WebviewWindow, width: f64) -> Option<MascotDockSide> {
    let monitor = window.current_monitor().ok().flatten()?;
    let scale = monitor.scale_factor();
    let work_size = monitor.work_area().size.to_logical::<f64>(scale);
    let work_pos = monitor.work_area().position.to_logical::<f64>(scale);
    let position = window.outer_position().ok()?.to_logical::<f64>(scale);
    Some(nearest_dock_side(
        position.x,
        width,
        work_pos.x,
        work_size.width,
    ))
}

#[cfg(not(windows))]
fn clamp_position_to_work_area(
    window: &tauri::WebviewWindow,
    position: LogicalPosition<f64>,
    width: f64,
    height: f64,
) -> LogicalPosition<f64> {
    let Ok(Some(monitor)) = window.current_monitor() else {
        return position;
    };
    let scale = monitor.scale_factor();
    let work_size = monitor.work_area().size.to_logical::<f64>(scale);
    let work_pos = monitor.work_area().position.to_logical::<f64>(scale);
    clamp_position_to_rect(position, width, height, work_pos, work_size)
}

#[cfg(not(windows))]
fn fit_notification_size_to_work_area(
    window: &tauri::WebviewWindow,
    width: f64,
    height: f64,
) -> LogicalSize<f64> {
    let Ok(Some(monitor)) = window.current_monitor() else {
        return LogicalSize { width, height };
    };
    let scale = monitor.scale_factor();
    let work_size = monitor.work_area().size.to_logical::<f64>(scale);
    fit_notification_size_to_rect(width, height, work_size)
}

fn fit_notification_size_to_rect(
    width: f64,
    height: f64,
    work_size: LogicalSize<f64>,
) -> LogicalSize<f64> {
    // Prefer the normal screen margin, but on unusually small work areas keep
    // as much of the mascot as possible instead of letting the notification
    // window extend beyond the monitor and lose its top edge.
    let available_width =
        (work_size.width - SCREEN_MARGIN * 2.0).max(work_size.width.min(MASCOT_WIDTH));
    let available_height =
        (work_size.height - SCREEN_MARGIN * 2.0).max(work_size.height.min(MASCOT_HEIGHT));

    LogicalSize {
        width: width.min(available_width),
        height: height.min(available_height),
    }
}

fn clamp_position_to_rect(
    position: LogicalPosition<f64>,
    width: f64,
    height: f64,
    work_pos: LogicalPosition<f64>,
    work_size: LogicalSize<f64>,
) -> LogicalPosition<f64> {
    let min_x = work_pos.x + SCREEN_MARGIN;
    let min_y = work_pos.y + SCREEN_MARGIN;
    let max_x = work_pos.x + work_size.width - width - SCREEN_MARGIN;
    let max_y = work_pos.y + work_size.height - height - SCREEN_MARGIN;

    LogicalPosition {
        x: if max_x >= min_x {
            position.x.clamp(min_x, max_x)
        } else {
            work_pos.x + (work_size.width - width) / 2.0
        },
        y: if max_y >= min_y {
            position.y.clamp(min_y, max_y)
        } else {
            work_pos.y + (work_size.height - height) / 2.0
        },
    }
}

fn dock_mascot_immediately(
    window: &tauri::WebviewWindow,
    motion: &MascotDockMotion,
    width: f64,
    height: f64,
) {
    let _ = animate_mascot_dock(window.clone(), motion.clone(), width, height, false, true);
}

fn mascot_is_partly_offscreen(window: &tauri::WebviewWindow, width: f64) -> bool {
    let Ok(Some(monitor)) = window.current_monitor() else {
        return false;
    };
    let scale = monitor.scale_factor();
    let work_size = monitor.work_area().size.to_logical::<f64>(scale);
    let work_pos = monitor.work_area().position.to_logical::<f64>(scale);
    let Ok(position) = window.outer_position() else {
        return false;
    };
    let position = position.to_logical::<f64>(scale);
    peeked_dock_side(position.x, width, work_pos.x, work_pos.x + work_size.width).is_some()
}

fn peeked_dock_side(
    position_x: f64,
    width: f64,
    work_left: f64,
    work_right: f64,
) -> Option<MascotDockSide> {
    if position_x < work_left - 1.0 {
        Some(MascotDockSide::Left)
    } else if position_x + width > work_right + 1.0 {
        Some(MascotDockSide::Right)
    } else {
        None
    }
}

#[cfg(test)]
mod desktop_auth_callback_tests {
    use super::{
        cleanup_desktop_diagnostic_files, desktop_auth_callback_diagnostic_fields,
        find_desktop_auth_callback, DesktopDiagnosticCleanupResult, PendingDesktopAuthCallback,
    };
    use std::fs;

    #[test]
    fn callback_argument_accepts_windows_quotes_whitespace_and_case() {
        let args = vec![
            "HualiAIDesktopAssistant.exe".to_owned(),
            "  \"HUALI-AI-MASCOT://AUTH-CALLBACK?state=one&token=two&userId=three\"  ".to_owned(),
        ];

        assert_eq!(
            find_desktop_auth_callback(&args).as_deref(),
            Some("HUALI-AI-MASCOT://AUTH-CALLBACK?state=one&token=two&userId=three")
        );
    }

    #[test]
    fn unrelated_second_launch_does_not_overwrite_a_pending_callback() {
        let pending = PendingDesktopAuthCallback::default();
        let callback = vec![
            "HualiAIDesktopAssistant.exe".to_owned(),
            "huali-ai-mascot://auth-callback?state=one&token=two&userId=three".to_owned(),
        ];
        let unrelated = vec![
            "HualiAIDesktopAssistant.exe".to_owned(),
            "--show".to_owned(),
        ];

        assert!(pending.capture(&callback).is_some());
        assert!(pending.capture(&unrelated).is_none());
        let captured = pending
            .take()
            .expect("pending callback should be preserved");
        assert_eq!(captured.callback_url, callback.get(1).cloned());
        assert_eq!(captured.argument_count, 2);
    }

    #[test]
    fn distinct_callbacks_are_delivered_in_order_instead_of_overwriting_each_other() {
        let pending = PendingDesktopAuthCallback::default();
        let first = vec![
            "HualiAIDesktopAssistant.exe".to_owned(),
            "huali-ai-mascot://auth-callback?state=old&token=one&userId=user".to_owned(),
        ];
        let second = vec![
            "HualiAIDesktopAssistant.exe".to_owned(),
            "huali-ai-mascot://auth-callback?state=current&token=two&userId=user".to_owned(),
        ];

        pending.capture(&first);
        pending.capture(&second);

        assert_eq!(
            pending.take().and_then(|item| item.callback_url),
            first.get(1).cloned()
        );
        assert_eq!(
            pending.take().and_then(|item| item.callback_url),
            second.get(1).cloned()
        );
        assert!(pending.take().is_none());
    }

    #[test]
    fn formal_release_removes_only_existing_diagnostic_jsonl_files() {
        let directory = std::env::temp_dir().join(format!(
            "huali-ai-diagnostic-cleanup-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("temporary diagnostic directory should be created");
        let primary = directory.join("desktop-diagnostic.jsonl");
        let rotated = directory.join("desktop-diagnostic.jsonl.1");
        let retained = directory.join("retained-user-config.json");
        fs::write(&primary, b"primary").expect("primary fixture should be written");
        fs::write(&rotated, b"rotated").expect("rotated fixture should be written");
        fs::write(&retained, b"keep").expect("retained fixture should be written");

        assert_eq!(
            cleanup_desktop_diagnostic_files(&primary),
            DesktopDiagnosticCleanupResult {
                primary_existed: true,
                primary_removed: true,
                rotated_existed: true,
                rotated_removed: true,
            }
        );
        assert!(!primary.exists());
        assert!(!rotated.exists());
        assert!(retained.is_file());
        assert_eq!(
            cleanup_desktop_diagnostic_files(&primary),
            DesktopDiagnosticCleanupResult::default()
        );

        fs::remove_dir_all(directory).expect("temporary diagnostic directory should be removed");
    }

    #[test]
    fn native_callback_diagnostics_preserve_the_prefix_remainder_and_raw_values() {
        let callback_url = "huali-ai-mascot://auth-callback.extra?state=complete-state&token=complete-token&userId=employee-10002";
        let fields = desktop_auth_callback_diagnostic_fields(callback_url);

        assert_eq!(fields["callbackUrl"], callback_url);
        assert_eq!(fields["callbackPrefixMatches"], true);
        assert_eq!(fields["callbackPrefixBoundary"], ".");
        assert_eq!(fields["callbackPrefixRemainderBeforeQuery"], ".extra");
        assert_eq!(fields["state"], "complete-state");
        assert_eq!(fields["token"], "complete-token");
        assert_eq!(fields["userId"], "employee-10002");
    }
}

#[cfg(test)]
mod mascot_position_tests {
    use super::{
        align_window_to_avatar, async_key_state_is_pressed, clamp_position_to_rect,
        fit_notification_size_to_rect, fit_panel_height_to_rect, mascot_avatar_offset,
        mascot_avatar_physical_rect, mascot_bottom_right_position,
        mascot_context_menu_physical_geometry, mascot_dock_eased_progress,
        mascot_dock_physical_target, mascot_dock_x, nearest_dock_side, notification_drag_delta,
        notification_physical_geometry, panel_physical_geometry, peeked_dock_side,
        physical_rect_contains, physical_rect_intersection, system_notification_physical_geometry,
        LogicalPosition, LogicalSize, MascotContextMenuPlacement, MascotContextMenuState,
        MascotDockSide, MascotSystemNotificationState, PanelActivityState, PanelLayoutState,
        PhysicalPosition, PhysicalRect, PhysicalSize, MASCOT_AUTH_NOTIFICATION_HEIGHT,
        MASCOT_AVATAR_HEIGHT, MASCOT_AVATAR_WIDTH, MASCOT_CONTEXT_MENU_ABOVE_VISIBLE_BOTTOM,
        MASCOT_CONTEXT_MENU_BELOW_VISIBLE_TOP, MASCOT_CONTEXT_MENU_GAP, MASCOT_CONTEXT_MENU_HEIGHT,
        MASCOT_CONTEXT_MENU_TAIL_MAX, MASCOT_CONTEXT_MENU_TAIL_MIN, MASCOT_CONTEXT_MENU_WIDTH,
        MASCOT_HEIGHT, MASCOT_MESSAGE_HEIGHT, MASCOT_MESSAGE_WIDTH,
        MASCOT_NOTIFICATION_BOTTOM_PADDING, MASCOT_NOTIFICATION_HEIGHT, MASCOT_NOTIFICATION_WIDTH,
        MASCOT_PEEK_VISIBLE_WIDTH, MASCOT_REST_BOTTOM_MARGIN, MASCOT_REST_RIGHT_MARGIN,
        MASCOT_SYSTEM_NOTIFICATION_GAP, MASCOT_SYSTEM_NOTIFICATION_HEIGHT,
        MASCOT_SYSTEM_NOTIFICATION_MARGIN, MASCOT_SYSTEM_NOTIFICATION_WIDTH, MASCOT_WIDTH,
        PANEL_COMPACT_HEIGHT, PANEL_MAX_HEIGHT, SCREEN_MARGIN,
    };

    #[test]
    fn initial_mascot_position_uses_work_area_and_safe_edge_margins() {
        let work_pos = LogicalPosition { x: 1920.0, y: 0.0 };
        let work_size = LogicalSize {
            width: 1366.0,
            // The Windows taskbar is already excluded from this work area.
            height: 728.0,
        };
        let position = mascot_bottom_right_position(work_pos, work_size);

        assert_eq!(
            position.x,
            work_pos.x + work_size.width - MASCOT_WIDTH - MASCOT_REST_RIGHT_MARGIN
        );
        assert_eq!(
            position.y,
            work_pos.y + work_size.height - MASCOT_HEIGHT - MASCOT_REST_BOTTOM_MARGIN
        );
    }

    #[test]
    fn visible_dragged_positions_are_never_forced_back_to_the_initial_dock() {
        assert_eq!(peeked_dock_side(420.0, 168.0, 0.0, 1920.0), None);
        assert_eq!(peeked_dock_side(1722.0, 168.0, 0.0, 1920.0), None);
    }

    #[test]
    fn deliberate_peeks_are_detected_on_both_edges() {
        assert_eq!(
            peeked_dock_side(-72.0, 168.0, 0.0, 1920.0),
            Some(MascotDockSide::Left)
        );
        assert_eq!(
            peeked_dock_side(1824.0, 168.0, 0.0, 1920.0),
            Some(MascotDockSide::Right)
        );
    }

    #[test]
    fn hover_hit_testing_uses_only_the_avatar_pixels_visible_inside_the_work_area() {
        let work_area = PhysicalRect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1040,
        };
        let right_peeked_avatar = PhysicalRect {
            x: 1848,
            y: 820,
            width: 96,
            height: 88,
        };
        let visible = physical_rect_intersection(right_peeked_avatar, work_area)
            .expect("the half-head must retain a visible hover target");

        assert_eq!(
            visible,
            PhysicalRect {
                x: 1848,
                y: 820,
                width: 72,
                height: 88
            }
        );
        assert!(physical_rect_contains(visible, 1900, 860));
        assert!(!physical_rect_contains(visible, 1830, 860));
        assert!(!physical_rect_contains(visible, 1920, 860));
    }

    #[test]
    fn nearest_screen_edge_controls_the_hide_direction() {
        assert_eq!(
            nearest_dock_side(120.0, MASCOT_WIDTH, 0.0, 1920.0),
            MascotDockSide::Left
        );
        assert_eq!(
            nearest_dock_side(1600.0, MASCOT_WIDTH, 0.0, 1920.0),
            MascotDockSide::Right
        );

        assert_eq!(
            mascot_dock_x(MascotDockSide::Left, true, MASCOT_WIDTH, 0.0, 1920.0),
            -MASCOT_WIDTH + MASCOT_PEEK_VISIBLE_WIDTH
        );
        assert_eq!(
            mascot_dock_x(MascotDockSide::Right, true, MASCOT_WIDTH, 0.0, 1920.0),
            1920.0 - MASCOT_PEEK_VISIBLE_WIDTH
        );
    }

    #[test]
    fn dock_motion_easing_is_monotonic_and_settles_at_both_ends() {
        for peek in [false, true] {
            assert_eq!(mascot_dock_eased_progress(peek, 0.0), 0.0);
            assert_eq!(mascot_dock_eased_progress(peek, 1.0), 1.0);

            let samples: Vec<f64> = (0..=40)
                .map(|step| mascot_dock_eased_progress(peek, f64::from(step) / 40.0))
                .collect();
            assert!(samples.windows(2).all(|pair| pair[1] >= pair[0]));
            assert!(samples[1] < 0.01, "motion must not jump on its first frame");
            assert!(1.0 - samples[39] < 0.01, "motion must settle gently");
        }
    }

    #[test]
    fn physical_dock_target_keeps_the_requested_width_visible_at_high_dpi() {
        for scale in [1.25_f64, 1.5, 1.75, 2.0] {
            let work_area = PhysicalRect {
                x: 0,
                y: 0,
                width: (2560.0 * scale).round() as u32,
                height: (1400.0 * scale).round() as u32,
            };
            let window_size = PhysicalSize {
                width: (MASCOT_WIDTH * scale).round() as u32,
                height: (MASCOT_HEIGHT * scale).round() as u32,
            };
            let visible = (MASCOT_PEEK_VISIBLE_WIDTH * scale).round() as i32;
            let (left, left_side) = mascot_dock_physical_target(
                PhysicalPosition { x: 40, y: 800 },
                window_size,
                work_area,
                scale,
                true,
            );
            let (right, right_side) = mascot_dock_physical_target(
                PhysicalPosition {
                    x: work_area.width as i32 - window_size.width as i32 - 40,
                    y: 800,
                },
                window_size,
                work_area,
                scale,
                true,
            );

            assert_eq!(left_side, MascotDockSide::Left);
            assert_eq!(right_side, MascotDockSide::Right);
            assert_eq!(left.x + window_size.width as i32, visible);
            assert_eq!(work_area.width as i32 - right.x, visible);
        }
    }

    #[test]
    fn native_mouse_state_uses_the_high_order_pressed_bit() {
        assert!(async_key_state_is_pressed(i16::MIN));
        assert!(async_key_state_is_pressed(-1));
        assert!(!async_key_state_is_pressed(0));
        assert!(!async_key_state_is_pressed(1));
    }

    #[test]
    fn visible_todo_panel_activity_blocks_native_idle_hiding() {
        let state = PanelActivityState::default();
        assert!(!state.is_engaged());

        state.set(true, false);
        assert!(state.is_engaged());

        state.set(false, true);
        assert!(state.is_engaged());

        state.set(false, false);
        assert!(!state.is_engaged());
    }

    #[test]
    fn expanded_notification_is_kept_inside_the_monitor_work_area() {
        let work_pos = LogicalPosition { x: 0.0, y: 0.0 };
        let work_size = LogicalSize {
            width: 600.0,
            height: 720.0,
        };
        let clamped = clamp_position_to_rect(
            LogicalPosition { x: 326.0, y: 210.0 },
            MASCOT_NOTIFICATION_WIDTH,
            MASCOT_NOTIFICATION_HEIGHT,
            work_pos,
            work_size,
        );

        assert_eq!(clamped.x, 600.0 - MASCOT_NOTIFICATION_WIDTH - SCREEN_MARGIN);
        assert!(clamped.y >= SCREEN_MARGIN);
        assert!(clamped.y + MASCOT_NOTIFICATION_HEIGHT <= 720.0 - SCREEN_MARGIN);
    }

    #[test]
    fn notification_window_shrinks_to_fit_a_short_work_area() {
        let fitted = fit_notification_size_to_rect(
            MASCOT_NOTIFICATION_WIDTH,
            MASCOT_NOTIFICATION_HEIGHT,
            LogicalSize {
                width: 400.0,
                height: 420.0,
            },
        );

        assert_eq!(fitted.width, MASCOT_NOTIFICATION_WIDTH);
        assert_eq!(fitted.height, 420.0 - SCREEN_MARGIN * 2.0);
    }

    #[test]
    fn notification_window_fits_a_1366_by_768_laptop_at_150_percent_scaling() {
        // A 1366x768 display with a 40px taskbar exposes roughly this logical
        // work area at 150% Windows scaling.
        let work_size = LogicalSize {
            width: 1366.0 / 1.5,
            height: (768.0 - 40.0) / 1.5,
        };
        let fitted = fit_notification_size_to_rect(
            MASCOT_NOTIFICATION_WIDTH,
            MASCOT_NOTIFICATION_HEIGHT,
            work_size,
        );

        assert_eq!(fitted.width, MASCOT_NOTIFICATION_WIDTH);
        assert_eq!(fitted.height, work_size.height - SCREEN_MARGIN * 2.0);
        assert!(fitted.height < MASCOT_NOTIFICATION_HEIGHT);
    }

    #[test]
    fn notification_window_keeps_its_designed_size_when_space_is_available() {
        let fitted = fit_notification_size_to_rect(
            MASCOT_NOTIFICATION_WIDTH,
            MASCOT_NOTIFICATION_HEIGHT,
            LogicalSize {
                width: 1920.0,
                height: 1040.0,
            },
        );

        assert_eq!(fitted.width, MASCOT_NOTIFICATION_WIDTH);
        assert_eq!(fitted.height, MASCOT_NOTIFICATION_HEIGHT);
    }

    #[test]
    fn notification_physical_geometry_uses_target_monitor_pixels_at_supported_dpi() {
        for scale in [1.0_f64, 1.25, 1.5, 1.75, 2.0] {
            let work_area = PhysicalRect {
                x: -(1920.0 * scale).round() as i32,
                y: -(40.0 * scale).round() as i32,
                width: (1920.0 * scale).round() as u32,
                height: (1040.0 * scale).round() as u32,
            };
            let avatar = PhysicalRect {
                x: work_area.x + (900.0 * scale).round() as i32,
                y: work_area.y + (760.0 * scale).round() as i32,
                width: (MASCOT_AVATAR_WIDTH * scale).round() as u32,
                height: (MASCOT_AVATAR_HEIGHT * scale).round() as u32,
            };

            for (visible, compact, expected_width, expected_height) in [
                (false, false, MASCOT_WIDTH, MASCOT_HEIGHT),
                (true, true, MASCOT_MESSAGE_WIDTH, MASCOT_MESSAGE_HEIGHT),
                (
                    true,
                    false,
                    MASCOT_NOTIFICATION_WIDTH,
                    MASCOT_NOTIFICATION_HEIGHT,
                ),
            ] {
                let geometry =
                    notification_physical_geometry(avatar, work_area, scale, visible, compact);
                let offset =
                    mascot_avatar_offset(expected_width, expected_height, visible, compact);
                let anchored_x = geometry.position.x + (offset.x * scale).round() as i32;
                let anchored_y = geometry.position.y + (offset.y * scale).round() as i32;
                let margin = (SCREEN_MARGIN * scale).round() as i32;

                assert_eq!(
                    geometry.size,
                    PhysicalSize {
                        width: (expected_width * scale).round() as u32,
                        height: (expected_height * scale).round() as u32,
                    }
                );
                assert!((anchored_x - avatar.x).abs() <= 1);
                assert!((anchored_y - avatar.y).abs() <= 1);
                assert!(geometry.position.x >= work_area.x + margin);
                assert!(geometry.position.y >= work_area.y + margin);
                assert!(
                    geometry.position.x + geometry.size.width as i32
                        <= work_area.x + work_area.width as i32 - margin
                );
                assert!(
                    geometry.position.y + geometry.size.height as i32
                        <= work_area.y + work_area.height as i32 - margin
                );
            }
        }
    }

    #[test]
    fn collapsed_geometry_follows_the_actual_avatar_after_edge_clamping() {
        let scale = 1.5;
        let work_area = PhysicalRect {
            x: -2048,
            y: 0,
            width: 2048,
            height: 1112,
        };
        let requested_avatar = PhysicalRect {
            x: -80,
            y: 1010,
            width: (MASCOT_AVATAR_WIDTH * scale).round() as u32,
            height: (MASCOT_AVATAR_HEIGHT * scale).round() as u32,
        };
        let expanded =
            notification_physical_geometry(requested_avatar, work_area, scale, true, false);
        let expanded_offset = mascot_avatar_offset(
            MASCOT_NOTIFICATION_WIDTH,
            MASCOT_NOTIFICATION_HEIGHT,
            true,
            false,
        );
        let clamped_avatar = PhysicalRect {
            x: expanded.position.x + (expanded_offset.x * scale).round() as i32,
            y: expanded.position.y + (expanded_offset.y * scale).round() as i32,
            width: requested_avatar.width,
            height: requested_avatar.height,
        };
        let collapsed =
            notification_physical_geometry(clamped_avatar, work_area, scale, false, false);
        let collapsed_offset = mascot_avatar_offset(MASCOT_WIDTH, MASCOT_HEIGHT, false, false);

        assert_ne!(clamped_avatar.x, requested_avatar.x);
        assert_ne!(clamped_avatar.y, requested_avatar.y);
        assert!(
            (collapsed.position.x + (collapsed_offset.x * scale).round() as i32 - clamped_avatar.x)
                .abs()
                <= 1
        );
        assert!(
            (collapsed.position.y + (collapsed_offset.y * scale).round() as i32 - clamped_avatar.y)
                .abs()
                <= 1
        );
    }

    #[test]
    fn todo_panel_grows_with_wrapped_text_without_exceeding_its_limit() {
        assert_eq!(fit_panel_height_to_rect(78.0, 720.0), PANEL_COMPACT_HEIGHT);
        assert_eq!(fit_panel_height_to_rect(178.0, 720.0), 178.0);
        assert_eq!(fit_panel_height_to_rect(500.0, 720.0), PANEL_MAX_HEIGHT);
    }

    #[test]
    fn todo_panel_is_clamped_inside_a_short_laptop_work_area() {
        assert_eq!(
            fit_panel_height_to_rect(500.0, 220.0),
            220.0 - SCREEN_MARGIN * 2.0
        );
    }

    #[test]
    fn panel_physical_size_uses_the_mascot_target_monitor_scale() {
        let avatar = PhysicalRect {
            x: 500,
            y: 900,
            width: 120,
            height: 110,
        };
        let work_area = PhysicalRect {
            x: 0,
            y: 0,
            width: 2560,
            height: 1440,
        };

        let at_125 = panel_physical_geometry(avatar, work_area, 1.25, 178.0);
        let at_200 = panel_physical_geometry(avatar, work_area, 2.0, 178.0);

        assert_eq!(
            at_125.size,
            PhysicalSize {
                width: 475,
                height: 223
            }
        );
        assert_eq!(
            at_200.size,
            PhysicalSize {
                width: 760,
                height: 356
            }
        );
    }

    #[test]
    fn panel_physical_geometry_supports_negative_coordinate_secondary_monitors() {
        let work_area = PhysicalRect {
            x: -3840,
            y: -120,
            width: 3840,
            height: 2040,
        };
        let left_edge = panel_physical_geometry(
            PhysicalRect {
                x: -3830,
                y: 1500,
                width: 192,
                height: 176,
            },
            work_area,
            2.0,
            240.0,
        );
        let right_edge = panel_physical_geometry(
            PhysicalRect {
                x: -110,
                y: 1500,
                width: 192,
                height: 176,
            },
            work_area,
            2.0,
            240.0,
        );

        assert_eq!(left_edge.position.x, -3840 + 48);
        assert_eq!(right_edge.position.x, -760 - 48);
        assert_eq!(
            left_edge.size,
            PhysicalSize {
                width: 760,
                height: 480
            }
        );
        assert!(left_edge.position.y >= work_area.y + 48);
        assert!(left_edge.position.y + left_edge.size.height as i32 <= 1920 - 48);
    }

    #[test]
    fn panel_layout_state_keeps_logical_height_independent_of_hidden_window_dpi() {
        let state = PanelLayoutState::default();
        assert_eq!(state.height(), PANEL_COMPACT_HEIGHT);
        state.set_height(178.0);
        assert_eq!(state.height(), 178.0);
    }

    #[test]
    fn notification_and_collapsed_windows_share_the_same_avatar_anchor() {
        let collapsed_position = LogicalPosition {
            x: 1722.0,
            y: 858.0,
        };
        let collapsed_offset = mascot_avatar_offset(MASCOT_WIDTH, MASCOT_HEIGHT, false, false);
        let notification_offset = mascot_avatar_offset(
            MASCOT_NOTIFICATION_WIDTH,
            MASCOT_NOTIFICATION_HEIGHT,
            true,
            false,
        );
        let notification_position =
            align_window_to_avatar(collapsed_position, collapsed_offset, notification_offset);
        let restored_position =
            align_window_to_avatar(notification_position, notification_offset, collapsed_offset);

        assert_eq!(restored_position, collapsed_position);
    }

    #[test]
    fn collapsed_window_stays_under_a_clamped_notification_avatar() {
        let collapsed_offset = mascot_avatar_offset(MASCOT_WIDTH, MASCOT_HEIGHT, false, false);
        let notification_offset = mascot_avatar_offset(
            MASCOT_NOTIFICATION_WIDTH,
            MASCOT_NOTIFICATION_HEIGHT,
            true,
            false,
        );
        let clamped_notification_position = LogicalPosition {
            x: 1576.0,
            y: 376.0,
        };
        let restored_position = align_window_to_avatar(
            clamped_notification_position,
            notification_offset,
            collapsed_offset,
        );

        assert_eq!(
            restored_position.x + collapsed_offset.x,
            clamped_notification_position.x + notification_offset.x
        );
        assert_eq!(
            restored_position.y + collapsed_offset.y,
            clamped_notification_position.y + notification_offset.y
        );
    }

    #[test]
    fn compact_bubbles_keep_the_avatar_at_the_collapsed_anchor() {
        let collapsed_position = LogicalPosition { x: 500.0, y: 420.0 };
        let collapsed_offset = mascot_avatar_offset(MASCOT_WIDTH, MASCOT_HEIGHT, false, false);
        let compact_offset =
            mascot_avatar_offset(MASCOT_MESSAGE_WIDTH, MASCOT_MESSAGE_HEIGHT, true, true);
        let compact_position =
            align_window_to_avatar(collapsed_position, collapsed_offset, compact_offset);

        assert_eq!(
            compact_position.x + compact_offset.x,
            collapsed_position.x + collapsed_offset.x
        );
        assert_eq!(
            compact_position.y + compact_offset.y,
            collapsed_position.y + collapsed_offset.y
        );
    }

    #[test]
    fn compact_menu_fits_common_windows_laptop_work_areas() {
        for (screen_width, screen_height, taskbar_height, scale) in [
            (1366.0, 768.0, 40.0, 1.25),
            (1366.0, 768.0, 40.0, 1.5),
            (1920.0, 1080.0, 48.0, 1.25),
            (1920.0, 1080.0, 48.0, 1.5),
            (2560.0, 1600.0, 48.0, 1.75),
            (2560.0, 1600.0, 48.0, 2.0),
        ] {
            let fitted = fit_notification_size_to_rect(
                MASCOT_MESSAGE_WIDTH,
                MASCOT_MESSAGE_HEIGHT,
                LogicalSize {
                    width: screen_width / scale,
                    height: (screen_height - taskbar_height) / scale,
                },
            );

            assert_eq!(fitted.width, MASCOT_MESSAGE_WIDTH);
            assert_eq!(fitted.height, MASCOT_MESSAGE_HEIGHT);
        }
    }

    #[test]
    fn context_menu_position_keeps_mascot_window_stationary() {
        let client_origin = PhysicalPosition { x: 1500, y: 700 };
        let avatar = mascot_avatar_physical_rect(
            client_origin,
            PhysicalSize {
                width: MASCOT_WIDTH as u32,
                height: MASCOT_HEIGHT as u32,
            },
            1.0,
        );
        let geometry = mascot_context_menu_physical_geometry(
            avatar,
            PhysicalRect {
                x: 0,
                y: 0,
                width: 1920,
                height: 1040,
            },
            1.0,
            1,
        );

        assert_eq!(
            geometry.payload.placement,
            MascotContextMenuPlacement::Above
        );
        assert_eq!(client_origin, PhysicalPosition { x: 1500, y: 700 });
        assert_eq!(
            geometry.position.y + MASCOT_CONTEXT_MENU_ABOVE_VISIBLE_BOTTOM as i32,
            avatar.y - MASCOT_CONTEXT_MENU_GAP as i32
        );
        assert_eq!(
            geometry.position.x + MASCOT_CONTEXT_MENU_WIDTH as i32 / 2,
            avatar.x + avatar.width as i32 / 2
        );
        assert_eq!(geometry.payload.tail_x, 96.0);
    }

    #[test]
    fn context_menu_flips_below_the_mascot_near_the_top_edge() {
        let avatar = PhysicalRect {
            x: 112,
            y: 38,
            width: 96,
            height: 88,
        };
        let geometry = mascot_context_menu_physical_geometry(
            avatar,
            PhysicalRect {
                x: 0,
                y: 0,
                width: 1366,
                height: 728,
            },
            1.0,
            2,
        );

        assert_eq!(
            geometry.payload.placement,
            MascotContextMenuPlacement::Below
        );
        assert_eq!(
            geometry.position.y + MASCOT_CONTEXT_MENU_BELOW_VISIBLE_TOP as i32,
            avatar.y + avatar.height as i32 + MASCOT_CONTEXT_MENU_GAP as i32
        );
    }

    #[test]
    fn context_menu_uses_target_monitor_physical_pixels_at_fractional_dpi() {
        for scale in [1.0_f64, 1.25, 1.5, 1.75, 2.0] {
            let work_area = PhysicalRect {
                x: -(1920.0 * scale) as i32,
                y: 0,
                width: (1920.0 * scale) as u32,
                height: (1040.0 * scale) as u32,
            };
            let avatar = PhysicalRect {
                x: work_area.x + (12.0 * scale).round() as i32,
                y: (700.0 * scale).round() as i32,
                width: (MASCOT_AVATAR_WIDTH * scale).round() as u32,
                height: (MASCOT_AVATAR_HEIGHT * scale).round() as u32,
            };
            let geometry = mascot_context_menu_physical_geometry(avatar, work_area, scale, 3);
            let margin = (SCREEN_MARGIN * scale).round() as i32;

            assert_eq!(
                geometry.size.width,
                (MASCOT_CONTEXT_MENU_WIDTH * scale).round() as u32
            );
            assert_eq!(
                geometry.size.height,
                (MASCOT_CONTEXT_MENU_HEIGHT * scale).round() as u32
            );
            assert!(geometry.position.x >= work_area.x + margin);
            assert!(
                geometry.position.x + geometry.size.width as i32
                    <= work_area.x + work_area.width as i32 - margin
            );
            assert!(geometry.payload.tail_x >= MASCOT_CONTEXT_MENU_TAIL_MIN);
            assert!(geometry.payload.tail_x <= MASCOT_CONTEXT_MENU_TAIL_MAX);
        }
    }

    #[test]
    fn isolated_notification_uses_bounded_message_and_auth_heights() {
        for scale in [1.0_f64, 1.25, 1.5, 1.75, 2.0] {
            let work_area = PhysicalRect {
                x: 0,
                y: 0,
                width: (1920.0 * scale).round() as u32,
                height: (1040.0 * scale).round() as u32,
            };
            let avatar = PhysicalRect {
                x: (1800.0 * scale).round() as i32,
                y: (900.0 * scale).round() as i32,
                width: (MASCOT_AVATAR_WIDTH * scale).round() as u32,
                height: (MASCOT_AVATAR_HEIGHT * scale).round() as u32,
            };
            let margin = (MASCOT_SYSTEM_NOTIFICATION_MARGIN * scale).round() as i32;
            let gap = (MASCOT_SYSTEM_NOTIFICATION_GAP * scale).round() as i32;

            for (compact, expected_height) in [
                (false, MASCOT_SYSTEM_NOTIFICATION_HEIGHT),
                (true, MASCOT_AUTH_NOTIFICATION_HEIGHT),
            ] {
                let geometry =
                    system_notification_physical_geometry(avatar, work_area, scale, compact);
                assert_eq!(
                    geometry.size.width,
                    (MASCOT_SYSTEM_NOTIFICATION_WIDTH * scale).round() as u32
                );
                assert_eq!(
                    geometry.size.height,
                    (expected_height * scale).round() as u32
                );
                assert!(geometry.position.x >= work_area.x + margin);
                assert!(
                    geometry.position.x + geometry.size.width as i32
                        <= work_area.x + work_area.width as i32 - margin
                );
                assert!(
                    geometry.position.y + geometry.size.height as i32 <= avatar.y - gap,
                    "notification must stay entirely above the avatar at scale {scale}"
                );
            }
        }
    }

    #[test]
    fn detached_cards_never_cover_the_avatar_in_small_scaled_work_areas() {
        for scale in [1.0, 1.5, 2.0] {
            let work = PhysicalRect {
                x: -1366,
                y: 0,
                width: 1366,
                height: 728,
            };
            for (x, y) in [(-1300, 30), (-750, 330), (-250, 570)] {
                let avatar = PhysicalRect {
                    x,
                    y,
                    width: (92.0 * scale) as u32,
                    height: (76.0 * scale) as u32,
                };
                let notification =
                    system_notification_physical_geometry(avatar, work, scale, false);
                let panel = panel_physical_geometry(avatar, work, scale, 380.0);
                for (pos, size) in [
                    (notification.position, notification.size),
                    (panel.position, panel.size),
                ] {
                    assert!(pos.x >= work.x && pos.y >= work.y);
                    assert!(pos.x + size.width as i32 <= work.x + work.width as i32);
                    assert!(pos.y + size.height as i32 <= work.y + work.height as i32);
                    assert!(
                        pos.x + size.width as i32 <= avatar.x
                            || pos.x >= avatar.x + avatar.width as i32
                            || pos.y + size.height as i32 <= avatar.y
                            || pos.y >= avatar.y + avatar.height as i32
                    );
                }
            }
        }
    }

    #[test]
    fn outside_focus_preserves_content_but_not_an_empty_focused_panel() {
        let state = PanelActivityState::default();
        state.set(true, false);
        assert!(state.has_content());
        state.set(false, true);
        assert!(!state.has_content());
    }

    #[test]
    fn context_menu_tail_stays_inside_the_nav_when_window_is_edge_clamped() {
        let work_area = PhysicalRect {
            x: 0,
            y: 0,
            width: 1366,
            height: 728,
        };
        let left = mascot_context_menu_physical_geometry(
            PhysicalRect {
                x: 0,
                y: 400,
                width: MASCOT_AVATAR_WIDTH as u32,
                height: MASCOT_AVATAR_HEIGHT as u32,
            },
            work_area,
            1.0,
            4,
        );
        let right = mascot_context_menu_physical_geometry(
            PhysicalRect {
                x: 1366 - MASCOT_AVATAR_WIDTH as i32,
                y: 400,
                width: MASCOT_AVATAR_WIDTH as u32,
                height: MASCOT_AVATAR_HEIGHT as u32,
            },
            work_area,
            1.0,
            5,
        );

        assert_eq!(left.payload.tail_x, MASCOT_CONTEXT_MENU_TAIL_MIN);
        assert_eq!(right.payload.tail_x, MASCOT_CONTEXT_MENU_TAIL_MAX);
    }

    #[test]
    fn context_menu_anchor_uses_the_live_expanded_client_size() {
        let avatar = mascot_avatar_physical_rect(
            PhysicalPosition { x: 200, y: 100 },
            PhysicalSize {
                width: 400,
                height: 600,
            },
            1.25,
        );

        assert_eq!(avatar.x, 200 + (112.0_f64 * 1.25).round() as i32);
        assert_eq!(avatar.y, 100 + (384.0_f64 * 1.25).round() as i32);
        assert_eq!(avatar.width, (MASCOT_AVATAR_WIDTH * 1.25).round() as u32);
        assert_eq!(avatar.height, (MASCOT_AVATAR_HEIGHT * 1.25).round() as u32);
    }

    #[test]
    fn context_menu_state_waits_for_webview_ready_and_cancels_stale_show() {
        let state = MascotContextMenuState::default();
        let first_show = state.request_show().unwrap();
        assert!(!state.can_prepare(first_show));
        assert_eq!(state.mark_ready().unwrap(), Some(first_show));
        assert!(state.can_prepare(first_show));
        assert!(state.mark_awaiting_layout_ack(first_show));
        assert!(state.can_ack_layout(first_show));

        state.request_hide().unwrap();
        assert!(!state.can_prepare(first_show));
        assert!(!state.can_ack_layout(first_show));
        assert!(!state.snapshot().visible);

        let latest_show = state.request_show().unwrap();
        assert!(!state.can_prepare(first_show));
        assert!(state.can_prepare(latest_show));
        assert!(state.mark_awaiting_layout_ack(latest_show));
        assert!(state.mark_visible(latest_show));
        assert!(state.snapshot().visible);
    }

    #[test]
    fn context_menu_hide_before_ready_prevents_late_first_show() {
        let state = MascotContextMenuState::default();
        let stale_show = state.request_show().unwrap();
        state.request_hide().unwrap();

        assert_eq!(state.mark_ready().unwrap(), None);
        assert!(!state.can_prepare(stale_show));
        assert!(!state.snapshot().desired_visible);
    }

    #[test]
    fn stale_focus_loss_cannot_cancel_a_new_context_menu_generation() {
        let state = MascotContextMenuState::default();
        let first_show = state.request_show().unwrap();
        assert_eq!(state.mark_ready().unwrap(), Some(first_show));
        assert!(state.mark_awaiting_layout_ack(first_show));
        assert!(state.mark_visible(first_show));
        assert!(state.is_visible());

        let replacement_show = state.request_show().unwrap();
        assert!(!state.is_visible());
        assert!(state.can_prepare(replacement_show));
        assert!(!state.can_prepare(first_show));
        assert!(!state.can_ack_layout(first_show));
    }

    #[test]
    fn quick_show_hide_show_rejects_every_stale_layout_ack_and_timeout() {
        let state = MascotContextMenuState::default();
        state.mark_ready().unwrap();

        let first_show = state.request_show().unwrap();
        assert!(state.mark_awaiting_layout_ack(first_show));
        state.request_hide().unwrap();
        let latest_show = state.request_show().unwrap();
        assert!(state.mark_awaiting_layout_ack(latest_show));

        assert!(!state.can_ack_layout(first_show));
        assert!(state.can_ack_layout(latest_show));
        assert!(!state.expire_pending_show(first_show));
        assert!(state.snapshot().desired_visible);
        assert_eq!(state.snapshot().generation, latest_show);
    }

    #[test]
    fn repeated_notification_ready_is_idempotent() {
        let state = MascotSystemNotificationState::default();
        assert!(state.mark_ready().unwrap());
        let generation = state.request_show(false, Some(1)).unwrap().unwrap();
        assert!(state.mark_visible(generation, false));
        assert!(!state.mark_ready().unwrap());
        assert_eq!(state.visible_compact(), Some(false));
    }

    #[test]
    fn notification_show_hide_show_generations_reject_stale_native_transitions() {
        let state = MascotSystemNotificationState::default();
        assert_eq!(state.request_show(false, Some(1)).unwrap(), None);
        state.mark_ready().unwrap();

        let stale_show = state.request_show(false, Some(2)).unwrap().unwrap();
        assert!(state.can_show(stale_show, false));
        let stale_hide = state.request_hide(Some(3)).unwrap().unwrap();
        assert!(!state.can_show(stale_show, false));
        assert!(state.can_hide(stale_hide));

        assert_eq!(state.request_show(false, Some(2)).unwrap(), None);
        let latest_show = state.request_show(true, Some(4)).unwrap().unwrap();
        assert!(!state.can_hide(stale_hide));
        assert!(state.can_show(latest_show, true));
        assert!(state.mark_visible(latest_show, true));
        assert_eq!(state.visible_compact(), Some(true));

        let latest_hide = state.request_hide(Some(5)).unwrap().unwrap();
        assert!(!state.can_show(latest_show, true));
        assert!(state.can_hide(latest_hide));
        state.mark_physical_hidden();
        assert_eq!(state.visible_compact(), None);
    }

    #[test]
    fn repeated_notification_cycles_never_allow_a_delayed_show_to_revive_the_overlay() {
        let state = MascotSystemNotificationState::default();
        state.mark_ready().unwrap();

        for cycle in 0_u64..32 {
            let show_client_generation = cycle * 2 + 1;
            let hide_client_generation = show_client_generation + 1;
            let show_generation = state
                .request_show(false, Some(show_client_generation))
                .unwrap()
                .unwrap();
            assert!(state.mark_visible(show_generation, false));

            let hide_generation = state
                .request_hide(Some(hide_client_generation))
                .unwrap()
                .unwrap();
            assert!(state.can_hide(hide_generation));
            assert_eq!(
                state
                    .request_show(false, Some(show_client_generation))
                    .unwrap(),
                None,
                "cycle {cycle} accepted a delayed stale show"
            );
            state.mark_physical_hidden();
            assert_eq!(state.visible_compact(), None);
        }
    }

    #[test]
    fn every_mascot_layout_uses_the_same_bottom_safety_gutter() {
        for (width, height, visible, compact) in [
            (MASCOT_WIDTH, MASCOT_HEIGHT, false, false),
            (MASCOT_MESSAGE_WIDTH, MASCOT_MESSAGE_HEIGHT, true, true),
            (
                MASCOT_NOTIFICATION_WIDTH,
                MASCOT_NOTIFICATION_HEIGHT,
                true,
                false,
            ),
        ] {
            let offset = mascot_avatar_offset(width, height, visible, compact);
            let bottom_gutter = height - offset.y - MASCOT_AVATAR_HEIGHT;
            assert_eq!(bottom_gutter, MASCOT_NOTIFICATION_BOTTOM_PADDING);
        }
    }

    #[test]
    fn notification_resize_noise_does_not_move_the_restored_mascot() {
        let delta = notification_drag_delta(
            Some(LogicalPosition { x: 500.8, y: 419.2 }),
            LogicalPosition { x: 500.0, y: 420.0 },
        );

        assert_eq!(delta, LogicalPosition { x: 0.0, y: 0.0 });
    }

    #[test]
    fn intentional_notification_drag_is_preserved() {
        let delta = notification_drag_delta(
            Some(LogicalPosition { x: 506.0, y: 416.0 }),
            LogicalPosition { x: 500.0, y: 420.0 },
        );

        assert_eq!(delta, LogicalPosition { x: 6.0, y: -4.0 });
    }
}

fn restore_mascot_if_peeked(
    window: &tauri::WebviewWindow,
    motion: &MascotDockMotion,
    width: f64,
    height: f64,
) {
    // Normal clicks and events must keep the user's dragged position. Only a
    // window that is actually beyond the work area is the deliberate peeked
    // state and may be restored to the right edge.
    if mascot_is_partly_offscreen(window, width) {
        dock_mascot_immediately(window, motion, width, height);
    }
}

fn mascot_dock_eased_progress(peek: bool, progress: f64) -> f64 {
    let progress = progress.clamp(0.0, 1.0);
    if peek {
        // Smootherstep gives the longer hide motion a quiet start and landing.
        progress.powi(3) * (progress * (progress * 6.0 - 15.0) + 10.0)
    } else {
        // Smoothstep avoids the several-pixel first-frame jump of a quartic
        // ease-out while keeping the shorter reveal responsive and settled.
        progress.powi(2) * (3.0 - 2.0 * progress)
    }
}

#[cfg(not(windows))]
fn animate_mascot_dock(
    window: tauri::WebviewWindow,
    motion: MascotDockMotion,
    width: f64,
    height: f64,
    peek: bool,
    reduced_motion: bool,
) -> Option<MascotDockSide> {
    let side = current_mascot_dock_side(&window, width)?;
    let target = mascot_dock_target(&window, width, height, peek)?;
    let scale = window.scale_factor().unwrap_or(1.0);
    let Ok(start) = window.outer_position() else {
        let _ = window.set_position(Position::Logical(target));
        return Some(side);
    };
    let start = start.to_logical::<f64>(scale);
    animate_window_position(window, motion, start, target, peek, reduced_motion);
    Some(side)
}

#[cfg(windows)]
fn animate_mascot_dock(
    window: tauri::WebviewWindow,
    motion: MascotDockMotion,
    _width: f64,
    _height: f64,
    peek: bool,
    reduced_motion: bool,
) -> Option<MascotDockSide> {
    let monitor = window.current_monitor().ok().flatten()?;
    let work_area = monitor.work_area();
    let start = window.outer_position().ok()?;
    let window_size = window.outer_size().ok()?;
    let (target, side) = mascot_dock_physical_target(
        start,
        window_size,
        PhysicalRect {
            x: work_area.position.x,
            y: work_area.position.y,
            width: work_area.size.width,
            height: work_area.size.height,
        },
        monitor.scale_factor(),
        peek,
    );
    animate_window_position_physical(window, motion, start, target, peek, reduced_motion);
    Some(side)
}

#[cfg(any(not(windows), test))]
fn animate_window_position(
    window: tauri::WebviewWindow,
    motion: MascotDockMotion,
    start: LogicalPosition<f64>,
    target: LogicalPosition<f64>,
    peek: bool,
    reduced_motion: bool,
) {
    let animation_token = motion.cancel();

    if reduced_motion {
        let _ = window.set_position(Position::Logical(target));
        return;
    }

    let duration = Duration::from_millis(if peek {
        MASCOT_PEEK_ANIMATION_DURATION_MS
    } else {
        MASCOT_REVEAL_ANIMATION_DURATION_MS
    });

    thread::spawn(move || {
        let started_at = Instant::now();
        loop {
            if motion.0.load(Ordering::SeqCst) != animation_token {
                return;
            }

            let progress = (started_at.elapsed().as_secs_f64() / duration.as_secs_f64()).min(1.0);
            let eased = mascot_dock_eased_progress(peek, progress);
            let position = LogicalPosition {
                x: start.x + (target.x - start.x) * eased,
                y: start.y + (target.y - start.y) * eased,
            };
            let _ = window.set_position(Position::Logical(position));

            if progress >= 1.0 {
                break;
            }
            thread::sleep(Duration::from_millis(MASCOT_DOCK_ANIMATION_FRAME_MS));
        }

        if motion.0.load(Ordering::SeqCst) == animation_token {
            let _ = window.set_position(Position::Logical(target));
        }
    });
}

#[cfg(windows)]
fn animate_window_position_physical(
    window: tauri::WebviewWindow,
    motion: MascotDockMotion,
    start: PhysicalPosition<i32>,
    target: PhysicalPosition<i32>,
    peek: bool,
    reduced_motion: bool,
) {
    let animation_token = motion.cancel();

    if reduced_motion {
        let _ = window.set_position(Position::Physical(target));
        return;
    }

    let duration = Duration::from_millis(if peek {
        MASCOT_PEEK_ANIMATION_DURATION_MS
    } else {
        MASCOT_REVEAL_ANIMATION_DURATION_MS
    });

    thread::spawn(move || {
        let started_at = Instant::now();
        loop {
            if motion.0.load(Ordering::SeqCst) != animation_token {
                return;
            }

            let progress = (started_at.elapsed().as_secs_f64() / duration.as_secs_f64()).min(1.0);
            let eased = mascot_dock_eased_progress(peek, progress);
            let position = PhysicalPosition {
                x: (f64::from(start.x) + f64::from(target.x - start.x) * eased).round() as i32,
                y: (f64::from(start.y) + f64::from(target.y - start.y) * eased).round() as i32,
            };
            let _ = window.set_position(Position::Physical(position));

            if progress >= 1.0 {
                break;
            }
            thread::sleep(Duration::from_millis(MASCOT_DOCK_ANIMATION_FRAME_MS));
        }

        if motion.0.load(Ordering::SeqCst) == animation_token {
            let _ = window.set_position(Position::Physical(target));
        }
    });
}

fn mascot_avatar_offset(
    width: f64,
    height: f64,
    visible: bool,
    _compact: bool,
) -> LogicalPosition<f64> {
    LogicalPosition {
        x: (width - MASCOT_AVATAR_WIDTH) / 2.0,
        y: if visible {
            height - MASCOT_NOTIFICATION_BOTTOM_PADDING - MASCOT_AVATAR_HEIGHT
        } else {
            (height - MASCOT_AVATAR_HEIGHT) / 2.0
        },
    }
}

fn align_window_to_avatar(
    source_position: LogicalPosition<f64>,
    source_avatar_offset: LogicalPosition<f64>,
    target_avatar_offset: LogicalPosition<f64>,
) -> LogicalPosition<f64> {
    LogicalPosition {
        x: source_position.x + source_avatar_offset.x - target_avatar_offset.x,
        y: source_position.y + source_avatar_offset.y - target_avatar_offset.y,
    }
}

#[cfg(any(not(windows), test))]
fn notification_drag_delta(
    current_position: Option<LogicalPosition<f64>>,
    expanded_position: LogicalPosition<f64>,
) -> LogicalPosition<f64> {
    let Some(current_position) = current_position else {
        return LogicalPosition { x: 0.0, y: 0.0 };
    };
    let x = current_position.x - expanded_position.x;
    let y = current_position.y - expanded_position.y;

    LogicalPosition {
        x: if x.abs() <= MASCOT_NOTIFICATION_DRAG_EPSILON {
            0.0
        } else {
            x
        },
        y: if y.abs() <= MASCOT_NOTIFICATION_DRAG_EPSILON {
            0.0
        } else {
            y
        },
    }
}

#[cfg(not(windows))]
fn set_window_bounds(
    window: &tauri::WebviewWindow,
    position: Option<LogicalPosition<f64>>,
    width: f64,
    height: f64,
) -> Result<(), String> {
    window
        .set_size(Size::Logical(LogicalSize { width, height }))
        .map_err(|error| format!("failed to set logical window size: {error}"))?;
    if let Some(position) = position {
        window
            .set_position(Position::Logical(position))
            .map_err(|error| format!("failed to set logical window position: {error}"))?;
    }
    Ok(())
}

fn set_window_physical_bounds(
    window: &tauri::WebviewWindow,
    position: PhysicalPosition<i32>,
    size: PhysicalSize<u32>,
) -> Result<(), String> {
    #[cfg(windows)]
    if let Ok(hwnd) = window.hwnd() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SetWindowPos, SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_NOZORDER,
        };
        let updated = unsafe {
            SetWindowPos(
                hwnd.0,
                std::ptr::null_mut(),
                position.x,
                position.y,
                size.width.min(i32::MAX as u32) as i32,
                size.height.min(i32::MAX as u32) as i32,
                SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_NOZORDER,
            )
        };
        if updated != 0 {
            return Ok(());
        }
    }

    window
        .set_size(Size::Physical(size))
        .map_err(|error| format!("failed to set physical window size: {error}"))?;
    window
        .set_position(Position::Physical(position))
        .map_err(|error| format!("failed to set physical window position: {error}"))?;
    Ok(())
}

fn set_window_physical_position_if_changed(
    window: &tauri::WebviewWindow,
    position: PhysicalPosition<i32>,
) -> Result<(), String> {
    if matches!(window.outer_position(), Ok(current) if current == position) {
        return Ok(());
    }

    #[cfg(windows)]
    if let Ok(hwnd) = window.hwnd() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SetWindowPos, SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER,
        };
        let updated = unsafe {
            SetWindowPos(
                hwnd.0,
                std::ptr::null_mut(),
                position.x,
                position.y,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_NOSIZE | SWP_NOZORDER,
            )
        };
        if updated != 0 {
            return Ok(());
        }
    }

    window
        .set_position(Position::Physical(position))
        .map_err(|error| format!("failed to set physical window position: {error}"))
}

fn set_window_physical_geometry_if_changed(
    window: &tauri::WebviewWindow,
    position: PhysicalPosition<i32>,
    size: PhysicalSize<u32>,
) -> Result<(), String> {
    if matches!(window.outer_size(), Ok(current) if current == size) {
        return set_window_physical_position_if_changed(window, position);
    }

    set_window_physical_bounds(window, position, size)
}

#[cfg(windows)]
fn resize_mascot_for_notification(
    window: &tauri::WebviewWindow,
    motion: &MascotDockMotion,
    _layout_state: &MascotNotificationLayoutState,
    visible: bool,
    compact: bool,
    _reveal: bool,
    _reduced_motion: bool,
) -> Result<(), String> {
    motion.cancel();
    let geometry = notification_physical_geometry_for_mascot(window, visible, compact)?;
    // The target monitor's DPI owns both coordinates and size. One physical
    // SetWindowPos prevents the intermediate white/clipped frame produced by
    // separate logical size and position updates on mixed-DPI desktops.
    set_window_physical_bounds(window, geometry.position, geometry.size)
}

#[cfg(not(windows))]
fn resize_mascot_for_notification(
    window: &tauri::WebviewWindow,
    motion: &MascotDockMotion,
    layout_state: &MascotNotificationLayoutState,
    visible: bool,
    compact: bool,
    reveal: bool,
    reduced_motion: bool,
) -> Result<(), String> {
    let (requested_width, requested_height) = if !visible {
        (MASCOT_WIDTH, MASCOT_HEIGHT)
    } else if compact {
        (MASCOT_MESSAGE_WIDTH, MASCOT_MESSAGE_HEIGHT)
    } else {
        (MASCOT_NOTIFICATION_WIDTH, MASCOT_NOTIFICATION_HEIGHT)
    };
    let target_size = if visible {
        fit_notification_size_to_work_area(window, requested_width, requested_height)
    } else {
        LogicalSize {
            width: requested_width,
            height: requested_height,
        }
    };
    let target_width = target_size.width;
    let target_height = target_size.height;
    let scale = window.scale_factor().unwrap_or(1.0);
    let current_size = window
        .outer_size()
        .ok()
        .map(|size| size.to_logical::<f64>(scale));
    let current_position = window
        .outer_position()
        .ok()
        .map(|position| position.to_logical::<f64>(scale));
    let was_peeked = current_position
        .zip(current_size)
        .and_then(|(position, size)| {
            window.current_monitor().ok().flatten().map(|monitor| {
                let monitor_scale = monitor.scale_factor();
                let work_size = monitor.work_area().size.to_logical::<f64>(monitor_scale);
                let work_pos = monitor
                    .work_area()
                    .position
                    .to_logical::<f64>(monitor_scale);
                peeked_dock_side(
                    position.x,
                    size.width,
                    work_pos.x,
                    work_pos.x + work_size.width,
                )
                .is_some()
            })
        })
        .unwrap_or(false);

    motion.cancel();

    let collapsed_avatar_offset = mascot_avatar_offset(MASCOT_WIDTH, MASCOT_HEIGHT, false, false);
    let target_avatar_offset = mascot_avatar_offset(target_width, target_height, visible, compact);

    let next_position = if visible {
        let requested_restore_position = layout_state
            .0
            .lock()
            .ok()
            .and_then(|layout| layout.as_ref().map(|layout| layout.restore_position))
            .or_else(|| {
                if was_peeked {
                    mascot_dock_target(window, MASCOT_WIDTH, MASCOT_HEIGHT, false)
                } else {
                    current_position
                }
            });

        requested_restore_position.map(|requested_restore_position| {
            let candidate = align_window_to_avatar(
                requested_restore_position,
                collapsed_avatar_offset,
                target_avatar_offset,
            );
            let expanded_position =
                clamp_position_to_work_area(window, candidate, target_width, target_height);
            // If the large card had to be clamped at a monitor edge, the avatar
            // itself moved with it. Restore the small window around the avatar's
            // actual on-screen position instead of snapping back to the old one.
            let restore_position = align_window_to_avatar(
                expanded_position,
                target_avatar_offset,
                collapsed_avatar_offset,
            );
            if let Ok(mut layout) = layout_state.0.lock() {
                *layout = Some(MascotNotificationLayout {
                    restore_position,
                    expanded_position,
                });
            }
            expanded_position
        })
    } else {
        let saved_layout = layout_state
            .0
            .lock()
            .ok()
            .and_then(|mut layout| layout.take());
        saved_layout
            .map(|layout| {
                let drag_delta =
                    notification_drag_delta(current_position, layout.expanded_position);
                clamp_position_to_work_area(
                    window,
                    LogicalPosition {
                        x: layout.restore_position.x + drag_delta.x,
                        y: layout.restore_position.y + drag_delta.y,
                    },
                    MASCOT_WIDTH,
                    MASCOT_HEIGHT,
                )
            })
            .or_else(|| {
                current_position.zip(current_size).map(|(position, size)| {
                    let current_compact = size.width <= MASCOT_MESSAGE_WIDTH + 1.0
                        && size.height <= MASCOT_MESSAGE_HEIGHT + 1.0
                        && size.width > MASCOT_WIDTH + 1.0;
                    let current_visible =
                        size.width > MASCOT_WIDTH + 1.0 || size.height > MASCOT_HEIGHT + 1.0;
                    let current_avatar_offset = mascot_avatar_offset(
                        size.width,
                        size.height,
                        current_visible,
                        current_compact,
                    );
                    clamp_position_to_work_area(
                        window,
                        align_window_to_avatar(
                            position,
                            current_avatar_offset,
                            collapsed_avatar_offset,
                        ),
                        MASCOT_WIDTH,
                        MASCOT_HEIGHT,
                    )
                })
            })
    };

    if visible && reveal && was_peeked {
        window
            .set_size(Size::Logical(LogicalSize {
                width: target_width,
                height: target_height,
            }))
            .map_err(|error| format!("failed to set reveal window size: {error}"))?;
        if let Some(position) = next_position {
            show_window_without_activation(window)?;
            animate_window_position(
                window.clone(),
                motion.clone(),
                current_position.unwrap_or(position),
                position,
                false,
                reduced_motion,
            );
        }
    } else {
        // Updating position and size separately exposes an intermediate WebView
        // frame where the avatar is laid out against the wrong bounds. Windows
        // applies both values atomically here, so dismissing a card cannot make
        // the avatar flash at the expanded window's top-left corner.
        set_window_bounds(window, next_position, target_width, target_height)?;
    }
    Ok(())
}

fn fit_panel_height_to_rect(requested_height: f64, work_height: f64) -> f64 {
    let requested_height = if requested_height.is_finite() {
        requested_height
    } else {
        PANEL_COMPACT_HEIGHT
    };
    let available_height = (work_height - SCREEN_MARGIN * 2.0).max(PANEL_COMPACT_HEIGHT);
    requested_height
        .clamp(PANEL_COMPACT_HEIGHT, PANEL_MAX_HEIGHT)
        .min(available_height)
}

#[cfg(not(windows))]
fn fit_panel_height_to_work_area(window: &tauri::WebviewWindow, requested_height: f64) -> f64 {
    let Ok(Some(monitor)) = window.current_monitor() else {
        return fit_panel_height_to_rect(requested_height, f64::INFINITY);
    };
    let scale = monitor.scale_factor();
    let work_height = monitor.work_area().size.to_logical::<f64>(scale).height;
    fit_panel_height_to_rect(requested_height, work_height)
}

fn place_panel_near_mascot(
    panel: &tauri::WebviewWindow,
    mascot: &tauri::WebviewWindow,
    requested_height: f64,
) -> Result<(), String> {
    #[cfg(windows)]
    {
        let geometry = panel_physical_geometry_near_mascot(mascot, requested_height)?;
        // The target monitor's scale determines both size and position. Never
        // consult the hidden panel's stale DPI after a 125% <-> 200% move.
        set_window_physical_geometry_if_changed(panel, geometry.position, geometry.size)
    }

    #[cfg(not(windows))]
    {
        let height = fit_panel_height_to_work_area(mascot, requested_height);
        if let Some(position) = panel_position_near_mascot(mascot, height) {
            return set_window_bounds(panel, Some(position), PANEL_WIDTH, height);
        }

        place_bottom_right(panel, PANEL_WIDTH, height);
        Ok(())
    }
}

#[cfg(not(windows))]
fn panel_position_near_mascot(
    mascot: &tauri::WebviewWindow,
    height: f64,
) -> Option<LogicalPosition<f64>> {
    let mascot_pos = mascot.outer_position().ok()?;
    let scale = mascot.scale_factor().unwrap_or(1.0);
    let mascot_pos = mascot_pos.to_logical::<f64>(scale);
    let (mascot_width, _mascot_height) = mascot_logical_size(mascot);
    let (min_x, max_x, min_y, max_y) = if let Ok(Some(monitor)) = mascot.current_monitor() {
        let screen_size = monitor.work_area().size.to_logical::<f64>(scale);
        let screen_pos = monitor.work_area().position.to_logical::<f64>(scale);
        (
            screen_pos.x + SCREEN_MARGIN,
            screen_pos.x + screen_size.width - PANEL_WIDTH - SCREEN_MARGIN,
            screen_pos.y + SCREEN_MARGIN,
            screen_pos.y + screen_size.height - height - SCREEN_MARGIN,
        )
    } else {
        (SCREEN_MARGIN, f64::MAX, SCREEN_MARGIN, f64::MAX)
    };
    let raw_x = mascot_pos.x + (mascot_width - PANEL_WIDTH) / 2.0;
    let x = raw_x.clamp(min_x, max_x.max(min_x));
    let raw_y = mascot_pos.y - height + PANEL_GAP;
    let y = raw_y.clamp(min_y, max_y.max(min_y));
    Some(LogicalPosition { x, y })
}

#[cfg(any(windows, test))]
fn panel_physical_geometry(
    avatar: PhysicalRect,
    work_area: PhysicalRect,
    scale: f64,
    requested_height: f64,
) -> PanelPhysicalGeometry {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let geometry = card_physical_geometry(
        avatar,
        work_area,
        scale,
        PANEL_WIDTH,
        fit_panel_height_to_rect(requested_height, f64::from(work_area.height) / scale),
        0.0,
        SCREEN_MARGIN,
    );
    PanelPhysicalGeometry {
        position: geometry.position,
        size: geometry.size,
    }
}

#[cfg(windows)]
fn panel_physical_geometry_near_mascot(
    mascot: &tauri::WebviewWindow,
    requested_height: f64,
) -> Result<PanelPhysicalGeometry, String> {
    let mascot_scale = mascot
        .scale_factor()
        .map_err(|error| format!("failed to read mascot scale factor: {error}"))?;
    let client_origin = mascot_client_origin_physical(mascot)?;
    let client_size = mascot
        .inner_size()
        .map_err(|error| format!("failed to read mascot client size: {error}"))?;
    let avatar = mascot_avatar_physical_rect(client_origin, client_size, mascot_scale);
    let avatar_center_x = i64::from(avatar.x) + i64::from(avatar.width) / 2;
    let avatar_center_y = i64::from(avatar.y) + i64::from(avatar.height) / 2;
    let monitor = mascot
        .available_monitors()
        .map_err(|error| format!("failed to enumerate monitors: {error}"))?
        .into_iter()
        .find(|monitor| {
            let position = monitor.position();
            let size = monitor.size();
            let left = i64::from(position.x);
            let top = i64::from(position.y);
            avatar_center_x >= left
                && avatar_center_x < left + i64::from(size.width)
                && avatar_center_y >= top
                && avatar_center_y < top + i64::from(size.height)
        })
        .or_else(|| mascot.current_monitor().ok().flatten())
        .or_else(|| mascot.primary_monitor().ok().flatten())
        .ok_or_else(|| "no monitor is available for the mascot panel".to_string())?;
    let work_area = monitor.work_area();
    Ok(panel_physical_geometry(
        avatar,
        PhysicalRect {
            x: work_area.position.x,
            y: work_area.position.y,
            width: work_area.size.width,
            height: work_area.size.height,
        },
        monitor.scale_factor(),
        requested_height,
    ))
}

#[cfg(windows)]
fn sync_visible_panel_to_mascot(app: &tauri::AppHandle) {
    sync_panel_if_visible(app);
}

fn emit_panel_visibility(app: &tauri::AppHandle, visible: bool) {
    let _ = app.emit_to("mascot", PANEL_VISIBILITY_EVENT, visible);
}

fn hide_panel_and_notify(app: &tauri::AppHandle) -> bool {
    if let Some(panel) = app.get_webview_window("panel") {
        let hidden = hide_transparent_window_safely(&panel);
        emit_panel_visibility(app, false);
        return hidden;
    }
    false
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
enum MascotContextMenuPlacement {
    Above,
    Below,
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct MascotContextMenuPlacementPayload {
    generation: u64,
    placement: MascotContextMenuPlacement,
    // CSS consumes this as a logical coordinate inside the 192-DIP nav. It is
    // deliberately not relative to the 216-DIP transparent native window.
    tail_x: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct MascotContextMenuVisibilityPayload {
    visible: bool,
    restore_notification: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PhysicalRect {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

#[cfg(any(windows, test))]
fn physical_rect_intersection(first: PhysicalRect, second: PhysicalRect) -> Option<PhysicalRect> {
    let left = i64::from(first.x).max(i64::from(second.x));
    let top = i64::from(first.y).max(i64::from(second.y));
    let right = (i64::from(first.x) + i64::from(first.width))
        .min(i64::from(second.x) + i64::from(second.width));
    let bottom = (i64::from(first.y) + i64::from(first.height))
        .min(i64::from(second.y) + i64::from(second.height));
    if right <= left || bottom <= top {
        return None;
    }

    Some(PhysicalRect {
        x: left.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        y: top.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        width: (right - left).min(i64::from(u32::MAX)) as u32,
        height: (bottom - top).min(i64::from(u32::MAX)) as u32,
    })
}

#[cfg(any(windows, test))]
fn physical_rect_contains(rect: PhysicalRect, x: i32, y: i32) -> bool {
    let x = i64::from(x);
    let y = i64::from(y);
    x >= i64::from(rect.x)
        && x < i64::from(rect.x) + i64::from(rect.width)
        && y >= i64::from(rect.y)
        && y < i64::from(rect.y) + i64::from(rect.height)
}

#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PanelPhysicalGeometry {
    position: PhysicalPosition<i32>,
    size: PhysicalSize<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NotificationPhysicalGeometry {
    position: PhysicalPosition<i32>,
    size: PhysicalSize<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct MascotContextMenuGeometry {
    position: PhysicalPosition<i32>,
    size: PhysicalSize<u32>,
    payload: MascotContextMenuPlacementPayload,
}

fn logical_to_physical(value: f64, scale: f64) -> i64 {
    (value * scale.max(f64::EPSILON)).round() as i64
}

#[cfg(any(windows, test))]
fn notification_physical_geometry(
    avatar: PhysicalRect,
    work_area: PhysicalRect,
    scale: f64,
    visible: bool,
    compact: bool,
) -> NotificationPhysicalGeometry {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let (requested_width, requested_height) = if !visible {
        (MASCOT_WIDTH, MASCOT_HEIGHT)
    } else if compact {
        (MASCOT_MESSAGE_WIDTH, MASCOT_MESSAGE_HEIGHT)
    } else {
        (MASCOT_NOTIFICATION_WIDTH, MASCOT_NOTIFICATION_HEIGHT)
    };
    let work_size = LogicalSize {
        width: f64::from(work_area.width) / scale,
        height: f64::from(work_area.height) / scale,
    };
    let target_size = if visible {
        fit_notification_size_to_rect(requested_width, requested_height, work_size)
    } else {
        LogicalSize {
            width: requested_width,
            height: requested_height,
        }
    };
    let width = logical_to_physical(target_size.width, scale).clamp(1, i64::from(u32::MAX)) as u32;
    let height =
        logical_to_physical(target_size.height, scale).clamp(1, i64::from(u32::MAX)) as u32;
    let target_offset =
        mascot_avatar_offset(target_size.width, target_size.height, visible, compact);
    let desired_x = i64::from(avatar.x) - logical_to_physical(target_offset.x, scale);
    let desired_y = i64::from(avatar.y) - logical_to_physical(target_offset.y, scale);
    let margin = logical_to_physical(SCREEN_MARGIN, scale).max(0);
    let work_left = i64::from(work_area.x);
    let work_top = i64::from(work_area.y);
    let work_right = work_left + i64::from(work_area.width);
    let work_bottom = work_top + i64::from(work_area.height);
    let min_x = work_left + margin;
    let min_y = work_top + margin;
    let max_x = work_right - i64::from(width) - margin;
    let max_y = work_bottom - i64::from(height) - margin;
    let x = if max_x >= min_x {
        desired_x.clamp(min_x, max_x)
    } else {
        work_left + (i64::from(work_area.width) - i64::from(width)) / 2
    };
    let y = if max_y >= min_y {
        desired_y.clamp(min_y, max_y)
    } else {
        work_top + (i64::from(work_area.height) - i64::from(height)) / 2
    };

    NotificationPhysicalGeometry {
        position: PhysicalPosition {
            x: x.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
            y: y.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        },
        size: PhysicalSize { width, height },
    }
}

fn system_notification_physical_geometry(
    avatar: PhysicalRect,
    work_area: PhysicalRect,
    scale: f64,
    compact: bool,
) -> NotificationPhysicalGeometry {
    card_physical_geometry(
        avatar,
        work_area,
        scale,
        MASCOT_SYSTEM_NOTIFICATION_WIDTH,
        if compact {
            MASCOT_AUTH_NOTIFICATION_HEIGHT
        } else {
            MASCOT_SYSTEM_NOTIFICATION_HEIGHT
        },
        MASCOT_SYSTEM_NOTIFICATION_GAP,
        MASCOT_SYSTEM_NOTIFICATION_MARGIN,
    )
}

// Shared placement for detached cards. Prefer the established above/below
// positions; use a side only when neither vertical position fits. If the work
// area cannot fit a full card anywhere, constrain its text viewport, never
// clamp a full-height card over the avatar.
fn card_physical_geometry(
    avatar: PhysicalRect,
    work_area: PhysicalRect,
    scale: f64,
    width: f64,
    height: f64,
    gap: f64,
    margin: f64,
) -> NotificationPhysicalGeometry {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let margin = logical_to_physical(margin, scale).max(0);
    let gap = logical_to_physical(gap, scale).max(0);
    let left = i64::from(work_area.x) + margin;
    let top = i64::from(work_area.y) + margin;
    let right = i64::from(work_area.x) + i64::from(work_area.width) - margin;
    let bottom = i64::from(work_area.y) + i64::from(work_area.height) - margin;
    let width = logical_to_physical(width, scale).clamp(1, (right - left).max(1));
    let mut height = logical_to_physical(height, scale).clamp(1, (bottom - top).max(1));
    let avatar_left = i64::from(avatar.x);
    let avatar_right = avatar_left + i64::from(avatar.width);
    let avatar_top = i64::from(avatar.y);
    let avatar_bottom = avatar_top + i64::from(avatar.height);
    let above = (avatar_top - gap - top).max(0);
    let below = (bottom - avatar_bottom - gap).max(0);
    let center_x = avatar_left + i64::from(avatar.width) / 2;
    let mut x = (center_x - width / 2).clamp(left, (right - width).max(left));
    let y;
    if above >= height {
        y = avatar_top - gap - height;
    } else if below >= height {
        y = avatar_bottom + gap;
    } else if right - avatar_right - gap >= width || avatar_left - gap - left >= width {
        x = if right - avatar_right - gap >= width {
            avatar_right + gap
        } else {
            avatar_left - gap - width
        };
        y = (avatar_top + i64::from(avatar.height) / 2 - height / 2)
            .clamp(top, (bottom - height).max(top));
    } else if above >= below {
        height = height.min(above.max(1));
        y = (avatar_top - gap - height).max(top);
    } else {
        height = height.min(below.max(1));
        y = (avatar_bottom + gap).min((bottom - height).max(top));
    }
    NotificationPhysicalGeometry {
        position: PhysicalPosition {
            x: x.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
            y: y.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        },
        size: PhysicalSize {
            width: width as u32,
            height: height as u32,
        },
    }
}

fn mascot_context_menu_physical_geometry(
    avatar: PhysicalRect,
    work_area: PhysicalRect,
    scale: f64,
    generation: u64,
) -> MascotContextMenuGeometry {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let menu_width = logical_to_physical(MASCOT_CONTEXT_MENU_WIDTH, scale).max(1);
    let menu_height = logical_to_physical(MASCOT_CONTEXT_MENU_HEIGHT, scale).max(1);
    let margin = logical_to_physical(SCREEN_MARGIN, scale).max(0);
    let gap = logical_to_physical(MASCOT_CONTEXT_MENU_GAP, scale).max(0);
    let visible_bottom =
        logical_to_physical(MASCOT_CONTEXT_MENU_ABOVE_VISIBLE_BOTTOM, scale).max(0);
    let visible_top = logical_to_physical(MASCOT_CONTEXT_MENU_BELOW_VISIBLE_TOP, scale).max(0);

    let work_left = i64::from(work_area.x);
    let work_top = i64::from(work_area.y);
    let work_right = work_left + i64::from(work_area.width);
    let work_bottom = work_top + i64::from(work_area.height);
    let avatar_left = i64::from(avatar.x);
    let avatar_top = i64::from(avatar.y);
    let avatar_right = avatar_left + i64::from(avatar.width);
    let avatar_bottom = avatar_top + i64::from(avatar.height);
    let avatar_center_x = (avatar_left as f64 + avatar_right as f64) / 2.0;

    let min_x = work_left + margin;
    let max_x = work_right - menu_width - margin;
    let desired_x = (avatar_center_x - menu_width as f64 / 2.0).round() as i64;
    let x = if max_x >= min_x {
        desired_x.clamp(min_x, max_x)
    } else {
        work_left + (i64::from(work_area.width) - menu_width) / 2
    };

    // The outer menu window contains transparent gutters. Align the visible
    // tail edge, not the HWND edge, to the requested gap from the avatar.
    let above_y = avatar_top - gap - visible_bottom;
    let below_y = avatar_bottom + gap - visible_top;
    let min_y = work_top + margin;
    let max_y = work_bottom - menu_height - margin;
    let (y, placement) = if above_y >= min_y {
        (above_y, MascotContextMenuPlacement::Above)
    } else {
        (
            if max_y >= min_y {
                below_y.clamp(min_y, max_y)
            } else {
                work_top + (i64::from(work_area.height) - menu_height) / 2
            },
            MascotContextMenuPlacement::Below,
        )
    };

    let nav_left_physical = x as f64 + MASCOT_CONTEXT_MENU_NAV_LEFT * scale;
    let tail_x = ((avatar_center_x - nav_left_physical) / scale)
        .clamp(MASCOT_CONTEXT_MENU_TAIL_MIN, MASCOT_CONTEXT_MENU_TAIL_MAX);

    MascotContextMenuGeometry {
        position: PhysicalPosition {
            x: x.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
            y: y.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        },
        size: PhysicalSize {
            width: menu_width.min(i64::from(u32::MAX)) as u32,
            height: menu_height.min(i64::from(u32::MAX)) as u32,
        },
        payload: MascotContextMenuPlacementPayload {
            generation,
            placement,
            tail_x,
        },
    }
}

fn mascot_client_origin_physical(
    mascot: &tauri::WebviewWindow,
) -> Result<PhysicalPosition<i32>, String> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::POINT;
        use windows_sys::Win32::Graphics::Gdi::ClientToScreen;

        let hwnd = mascot
            .hwnd()
            .map_err(|error| format!("failed to access mascot HWND: {error}"))?;
        let mut point = POINT { x: 0, y: 0 };
        if unsafe { ClientToScreen(hwnd.0, &mut point) } != 0 {
            Ok(PhysicalPosition {
                x: point.x,
                y: point.y,
            })
        } else {
            Err("failed to convert mascot client origin to screen coordinates".to_string())
        }
    }

    #[cfg(not(windows))]
    mascot
        .outer_position()
        .map_err(|error| format!("failed to read mascot window position: {error}"))
}

fn mascot_avatar_physical_rect(
    client_origin: PhysicalPosition<i32>,
    client_size: PhysicalSize<u32>,
    scale: f64,
) -> PhysicalRect {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let logical_size = client_size.to_logical::<f64>(scale);
    let visible =
        logical_size.width > MASCOT_WIDTH + 1.0 || logical_size.height > MASCOT_HEIGHT + 1.0;
    let compact = visible
        && logical_size.width <= MASCOT_MESSAGE_WIDTH + 1.0
        && logical_size.height <= MASCOT_MESSAGE_HEIGHT + 1.0;
    let offset = mascot_avatar_offset(logical_size.width, logical_size.height, visible, compact);

    PhysicalRect {
        x: (i64::from(client_origin.x) + logical_to_physical(offset.x, scale))
            .clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        y: (i64::from(client_origin.y) + logical_to_physical(offset.y, scale))
            .clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        width: logical_to_physical(MASCOT_AVATAR_WIDTH, scale).clamp(1, i64::from(u32::MAX)) as u32,
        height: logical_to_physical(MASCOT_AVATAR_HEIGHT, scale).clamp(1, i64::from(u32::MAX))
            as u32,
    }
}

#[cfg(windows)]
fn monitor_mascot_peek_hover(
    app: tauri::AppHandle,
    hover_monitor: MascotPeekHoverMonitor,
    motion: MascotDockMotion,
    token: u64,
    reduced_motion: bool,
) {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;

    thread::spawn(move || {
        let started_at = Instant::now();
        let arm_after = Duration::from_millis(if reduced_motion {
            MASCOT_PEEK_HOVER_POLL_INTERVAL_MS
        } else {
            MASCOT_PEEK_ANIMATION_DURATION_MS
        });

        loop {
            if hover_monitor.0.load(Ordering::SeqCst) != token {
                return;
            }
            if started_at.elapsed() < arm_after {
                thread::sleep(Duration::from_millis(MASCOT_PEEK_HOVER_POLL_INTERVAL_MS));
                continue;
            }

            let Some(window) = app.get_webview_window("mascot") else {
                return;
            };
            if !matches!(window.is_visible(), Ok(true)) {
                return;
            }
            let (width, height) = mascot_logical_size(&window);
            if !mascot_is_partly_offscreen(&window, width) {
                // A newer show/reveal path restored the HWND without needing
                // this monitor. Do not let the old peek generation revive it.
                return;
            }

            let Ok(scale) = window.scale_factor() else {
                return;
            };
            let Ok(client_origin) = mascot_client_origin_physical(&window) else {
                return;
            };
            let Ok(client_size) = window.inner_size() else {
                return;
            };
            let Some(native_monitor) = window.current_monitor().ok().flatten() else {
                return;
            };
            let work_area = native_monitor.work_area();
            let avatar = mascot_avatar_physical_rect(client_origin, client_size, scale);
            let visible_avatar = physical_rect_intersection(
                avatar,
                PhysicalRect {
                    x: work_area.position.x,
                    y: work_area.position.y,
                    width: work_area.size.width,
                    height: work_area.size.height,
                },
            );
            let mut cursor = POINT { x: 0, y: 0 };
            if visible_avatar.is_some_and(|rect| {
                (unsafe { GetCursorPos(&mut cursor) }) != 0
                    && physical_rect_contains(rect, cursor.x, cursor.y)
            }) {
                hover_monitor.cancel();
                if !show_interactive_window(&window, false) {
                    return;
                }
                animate_mascot_dock(window, motion, width, height, false, reduced_motion);
                let _ = app.emit_to("mascot", MASCOT_NATIVE_HOVER_REVEALED_EVENT, ());
                return;
            }

            thread::sleep(Duration::from_millis(MASCOT_PEEK_HOVER_POLL_INTERVAL_MS));
        }
    });
}

#[cfg(windows)]
fn notification_physical_geometry_for_mascot(
    mascot: &tauri::WebviewWindow,
    visible: bool,
    compact: bool,
) -> Result<NotificationPhysicalGeometry, String> {
    let current_scale = mascot
        .scale_factor()
        .map_err(|error| format!("failed to read mascot scale factor: {error}"))?;
    let client_origin = mascot_client_origin_physical(mascot)?;
    let client_size = mascot
        .inner_size()
        .map_err(|error| format!("failed to read mascot client size: {error}"))?;
    let avatar = mascot_avatar_physical_rect(client_origin, client_size, current_scale);
    let avatar_center_x = i64::from(avatar.x) + i64::from(avatar.width) / 2;
    let avatar_center_y = i64::from(avatar.y) + i64::from(avatar.height) / 2;
    let monitor = mascot
        .available_monitors()
        .map_err(|error| format!("failed to enumerate monitors: {error}"))?
        .into_iter()
        .find(|monitor| {
            let position = monitor.position();
            let size = monitor.size();
            let left = i64::from(position.x);
            let top = i64::from(position.y);
            avatar_center_x >= left
                && avatar_center_x < left + i64::from(size.width)
                && avatar_center_y >= top
                && avatar_center_y < top + i64::from(size.height)
        })
        .or_else(|| mascot.current_monitor().ok().flatten())
        .or_else(|| mascot.primary_monitor().ok().flatten())
        .ok_or_else(|| "no monitor is available for the mascot notification".to_string())?;
    let work_area = monitor.work_area();
    Ok(notification_physical_geometry(
        avatar,
        PhysicalRect {
            x: work_area.position.x,
            y: work_area.position.y,
            width: work_area.size.width,
            height: work_area.size.height,
        },
        monitor.scale_factor(),
        visible,
        compact,
    ))
}

fn emit_mascot_context_menu_visibility(
    app: &tauri::AppHandle,
    visible: bool,
    restore_notification: bool,
) -> Result<(), String> {
    write_desktop_diagnostic_event(
        app,
        "interaction.context_menu.visibility_published",
        diagnostic_fields(serde_json::json!({
            "visible": visible,
            "restoreNotification": restore_notification,
        })),
    );
    app.emit_to(
        "mascot",
        MASCOT_CONTEXT_MENU_VISIBILITY_EVENT,
        MascotContextMenuVisibilityPayload {
            visible,
            restore_notification,
        },
    )
    .map_err(|error| format!("failed to publish mascot context menu visibility: {error}"))
}

fn hide_mascot_context_menu_native_window(app: &tauri::AppHandle) {
    if let Some(menu) = app.get_webview_window("mascot-menu") {
        // Disable hit testing before hiding. If the native hide ever fails, the
        // transparent topmost surface still cannot block the mascot or desktop.
        let _ = menu.set_ignore_cursor_events(true);
        let _ = menu.hide();
    }
}

// The caller holds `state.transition`. Only cancel the supplied generation so
// a stale placement, ACK or timeout can never hide a newer menu request.
fn rollback_mascot_context_menu_generation(
    app: &tauri::AppHandle,
    state: &MascotContextMenuState,
    generation: u64,
) -> bool {
    let cancelled = state.cancel_generation(generation);
    if cancelled {
        hide_mascot_context_menu_native_window(app);
    }
    if cancelled {
        let _ = emit_mascot_context_menu_visibility(app, false, true);
    }
    cancelled
}

fn hide_mascot_context_menu_window_with_restore(
    app: &tauri::AppHandle,
    restore_notification: bool,
) {
    let state = app.state::<MascotContextMenuState>();
    let Ok(_transition) = state.transition.lock() else {
        hide_mascot_context_menu_native_window(app);
        let _ = emit_mascot_context_menu_visibility(app, false, restore_notification);
        return;
    };
    let _ = state.request_hide();
    // On Windows the transparent menu HWND is shown off-screen briefly so its
    // WebView2 renderer can mount and publish ready. Hiding it before that IPC
    // arrives suspends navigation and makes the first real right-click wait
    // forever. Logical hide still wins; the warm-up surface remains off-screen
    // and click-through until ready closes it.
    #[cfg(windows)]
    if !state.is_ready() {
        let _ = emit_mascot_context_menu_visibility(app, false, restore_notification);
        return;
    }
    hide_mascot_context_menu_native_window(app);
    let _ = emit_mascot_context_menu_visibility(app, false, restore_notification);
}

fn hide_mascot_context_menu_window(app: &tauri::AppHandle) {
    hide_mascot_context_menu_window_with_restore(app, true);
}

// Phase one: calculate native bounds while the HWND remains hidden, then send
// the generation-scoped placement to Vue. Vue ACKs only after that layout has
// been painted, which prevents an above/below or edge-tail first-frame jump.
fn prepare_mascot_context_menu_generation(
    app: &tauri::AppHandle,
    state: &MascotContextMenuState,
    generation: u64,
) -> Result<bool, String> {
    if !state.can_prepare(generation) {
        return Ok(false);
    }

    let mascot = app
        .get_webview_window("mascot")
        .ok_or_else(|| "mascot window is unavailable".to_string())?;
    let menu = app
        .get_webview_window("mascot-menu")
        .ok_or_else(|| "mascot context menu window is unavailable".to_string())?;
    let scale = mascot
        .scale_factor()
        .map_err(|error| format!("failed to read mascot scale factor: {error}"))?;
    let client_origin = mascot_client_origin_physical(&mascot)?;
    let client_size = mascot
        .inner_size()
        .map_err(|error| format!("failed to read mascot client size: {error}"))?;
    let avatar = mascot_avatar_physical_rect(client_origin, client_size, scale);
    let avatar_center_x = i64::from(avatar.x) + i64::from(avatar.width) / 2;
    let avatar_center_y = i64::from(avatar.y) + i64::from(avatar.height) / 2;
    // An expanded transparent mascot window can span displays. Select the
    // monitor containing the actual avatar rather than the HWND's majority
    // area, otherwise a mixed-DPI boundary can apply the wrong work area and
    // scale to the independent menu window.
    let monitor = mascot
        .available_monitors()
        .map_err(|error| format!("failed to enumerate monitors: {error}"))?
        .into_iter()
        .find(|monitor| {
            let position = monitor.position();
            let size = monitor.size();
            let left = i64::from(position.x);
            let top = i64::from(position.y);
            avatar_center_x >= left
                && avatar_center_x < left + i64::from(size.width)
                && avatar_center_y >= top
                && avatar_center_y < top + i64::from(size.height)
        })
        .or_else(|| mascot.current_monitor().ok().flatten())
        .or_else(|| mascot.primary_monitor().ok().flatten())
        .ok_or_else(|| "no monitor is available for the mascot context menu".to_string())?;
    let work_area = monitor.work_area();
    let geometry = mascot_context_menu_physical_geometry(
        avatar,
        PhysicalRect {
            x: work_area.position.x,
            y: work_area.position.y,
            width: work_area.size.width,
            height: work_area.size.height,
        },
        monitor.scale_factor(),
        generation,
    );

    harden_transparent_window(&menu);
    set_window_physical_bounds(&menu, geometry.position, geometry.size)?;
    if !state.mark_awaiting_layout_ack(generation) {
        return Ok(false);
    }
    app.emit_to(
        "mascot-menu",
        "mascot-context-menu-placement",
        geometry.payload,
    )
    .map_err(|error| format!("failed to publish mascot context menu placement: {error}"))?;
    Ok(true)
}

fn schedule_mascot_context_menu_timeout(
    app: tauri::AppHandle,
    state: MascotContextMenuState,
    generation: u64,
) {
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(
            MASCOT_CONTEXT_MENU_LAYOUT_ACK_TIMEOUT_MS,
        ));
        let Ok(_transition) = state.transition.lock() else {
            hide_mascot_context_menu_native_window(&app);
            let _ = emit_mascot_context_menu_visibility(&app, false, true);
            return;
        };
        if state.expire_pending_show(generation) {
            hide_mascot_context_menu_native_window(&app);
            let _ = emit_mascot_context_menu_visibility(&app, false, true);
        }
    });
}

#[tauri::command]
fn show_mascot_context_menu(
    app: tauri::AppHandle,
    state: tauri::State<'_, MascotContextMenuState>,
) -> Result<bool, String> {
    hide_mascot_system_notification_native_window(&app);
    hide_panel_and_notify(&app);
    let _transition = state
        .transition
        .lock()
        .map_err(|_| "mascot context menu transition is unavailable".to_string())?;
    let generation = state.request_show()?;
    if state.can_prepare(generation) {
        hide_mascot_context_menu_native_window(&app);
        match prepare_mascot_context_menu_generation(&app, state.inner(), generation) {
            Ok(true) => {
                schedule_mascot_context_menu_timeout(app, state.inner().clone(), generation);
                return Ok(true);
            }
            Ok(false) => {
                rollback_mascot_context_menu_generation(&app, state.inner(), generation);
                return Ok(false);
            }
            Err(error) => {
                rollback_mascot_context_menu_generation(&app, state.inner(), generation);
                return Err(error);
            }
        }
    }
    // The Windows warm-up WebView will prepare this generation from its ready
    // command. Do not start the layout timeout until placement was emitted.
    Ok(true)
}

#[tauri::command]
fn ack_mascot_context_menu_layout(
    app: tauri::AppHandle,
    state: tauri::State<'_, MascotContextMenuState>,
    generation: u64,
) -> Result<bool, String> {
    let _transition = state
        .transition
        .lock()
        .map_err(|_| "mascot context menu transition is unavailable".to_string())?;
    if !state.can_ack_layout(generation) {
        return Ok(false);
    }
    let menu = match app.get_webview_window("mascot-menu") {
        Some(menu) => menu,
        None => {
            rollback_mascot_context_menu_generation(&app, state.inner(), generation);
            return Err("mascot context menu window is unavailable".to_string());
        }
    };
    harden_transparent_window(&menu);
    if let Err(error) = menu.show() {
        rollback_mascot_context_menu_generation(&app, state.inner(), generation);
        return Err(format!("failed to show mascot context menu: {error}"));
    }
    if let Err(error) = menu.set_ignore_cursor_events(false) {
        rollback_mascot_context_menu_generation(&app, state.inner(), generation);
        return Err(format!(
            "failed to restore mascot context menu hit testing: {error}"
        ));
    }
    if let Err(error) = menu.set_focus() {
        rollback_mascot_context_menu_generation(&app, state.inner(), generation);
        return Err(format!("failed to focus mascot context menu: {error}"));
    }
    if !state.mark_visible(generation) {
        // This is not expected while the transition lock is held. Generation-
        // scoped rollback avoids hiding a newer menu if the ACK became stale.
        rollback_mascot_context_menu_generation(&app, state.inner(), generation);
        return Ok(false);
    }
    if let Err(error) = emit_mascot_context_menu_visibility(&app, true, true) {
        rollback_mascot_context_menu_generation(&app, state.inner(), generation);
        return Err(error);
    }
    Ok(true)
}

#[tauri::command]
fn hide_mascot_context_menu(app: tauri::AppHandle) -> bool {
    hide_mascot_context_menu_window(&app);
    true
}

#[tauri::command]
fn set_mascot_context_menu_ready(
    app: tauri::AppHandle,
    state: tauri::State<'_, MascotContextMenuState>,
) -> Result<bool, String> {
    let _transition = state
        .transition
        .lock()
        .map_err(|_| "mascot context menu transition is unavailable".to_string())?;
    let Some(generation) = state.mark_ready()? else {
        if let Some(menu) = app.get_webview_window("mascot-menu") {
            menu.set_ignore_cursor_events(true)
                .map_err(|error| format!("failed to finish menu warm-up hit testing: {error}"))?;
            menu.hide()
                .map_err(|error| format!("failed to finish menu warm-up hide: {error}"))?;
        }
        return Ok(true);
    };
    match prepare_mascot_context_menu_generation(&app, state.inner(), generation) {
        Ok(true) => {
            schedule_mascot_context_menu_timeout(app, state.inner().clone(), generation);
            Ok(true)
        }
        Ok(false) => {
            rollback_mascot_context_menu_generation(&app, state.inner(), generation);
            Ok(false)
        }
        Err(error) => {
            rollback_mascot_context_menu_generation(&app, state.inner(), generation);
            Err(error)
        }
    }
}

fn hide_panel_after_focus_moves_outside_app(app: tauri::AppHandle) {
    thread::spawn(move || {
        // Focus settles after the mouse-down that moves it to another native
        // window. Keep the panel open when focus moved to the mascot itself;
        // that click will deliberately toggle or drag both windows.
        thread::sleep(Duration::from_millis(50));
        let mascot_is_focused = app
            .get_webview_window("mascot")
            .and_then(|window| window.is_focused().ok())
            .unwrap_or(false);
        if mascot_is_focused {
            return;
        }

        if app.state::<PanelActivityState>().has_content() {
            return;
        }
        if let Some(panel) = app.get_webview_window("panel") {
            if matches!(panel.is_focused(), Ok(true)) {
                return;
            }
            if matches!(panel.is_visible(), Ok(true)) {
                let _ = hide_transparent_window_safely(&panel);
                emit_panel_visibility(&app, false);
            }
        }
    });
}

fn hide_context_menu_after_focus_moves_outside_app(app: tauri::AppHandle) {
    thread::spawn(move || {
        // Focused(false) from a superseded generation can be delivered after a
        // new generation has already shown the same HWND. Read the settled
        // native focus instead of allowing that stale event to close the new
        // menu. A real click outside remains unfocused and closes normally.
        thread::sleep(Duration::from_millis(20));
        let menu_is_focused = app
            .get_webview_window("mascot-menu")
            .and_then(|window| window.is_focused().ok())
            .unwrap_or(false);
        if menu_is_focused {
            return;
        }

        if app.state::<MascotContextMenuState>().is_visible() {
            hide_mascot_context_menu_window(&app);
        }
    });
}

fn hide_mascot_system_notification_native_window_with_generation(
    app: &tauri::AppHandle,
    client_generation: Option<u64>,
) {
    let state = app.state::<MascotSystemNotificationState>();
    let generation = match state.request_hide(client_generation) {
        Ok(Some(generation)) => generation,
        Ok(None) => return,
        Err(_) => {
            if let Some(window) = app.get_webview_window("mascot-notification") {
                let _ = hide_transparent_window_safely(&window);
                state.mark_physical_hidden();
            }
            return;
        }
    };
    let Ok(_transition) = state.transition.lock() else {
        if let Some(window) = app.get_webview_window("mascot-notification") {
            let _ = hide_transparent_window_safely(&window);
            state.mark_physical_hidden();
        }
        return;
    };
    if !state.can_hide(generation) {
        return;
    }
    if let Some(window) = app.get_webview_window("mascot-notification") {
        let _ = hide_transparent_window_safely(&window);
    }
    state.mark_physical_hidden();
}

fn hide_mascot_system_notification_native_window(app: &tauri::AppHandle) {
    hide_mascot_system_notification_native_window_with_generation(app, None);
}

fn position_mascot_system_notification_window(
    mascot: &tauri::WebviewWindow,
    notification: &tauri::WebviewWindow,
    compact: bool,
) -> Result<(), String> {
    let mascot_scale = mascot
        .scale_factor()
        .map_err(|error| format!("failed to read mascot scale factor: {error}"))?;
    let client_origin = mascot_client_origin_physical(mascot)?;
    let client_size = mascot
        .inner_size()
        .map_err(|error| format!("failed to read mascot client size: {error}"))?;
    let avatar = mascot_avatar_physical_rect(client_origin, client_size, mascot_scale);
    let avatar_center_x = i64::from(avatar.x) + i64::from(avatar.width) / 2;
    let avatar_center_y = i64::from(avatar.y) + i64::from(avatar.height) / 2;
    let monitor = mascot
        .available_monitors()
        .map_err(|error| format!("failed to enumerate monitors: {error}"))?
        .into_iter()
        .find(|monitor| {
            let position = monitor.position();
            let size = monitor.size();
            let left = i64::from(position.x);
            let top = i64::from(position.y);
            avatar_center_x >= left
                && avatar_center_x < left + i64::from(size.width)
                && avatar_center_y >= top
                && avatar_center_y < top + i64::from(size.height)
        })
        .or_else(|| mascot.current_monitor().ok().flatten())
        .or_else(|| mascot.primary_monitor().ok().flatten())
        .ok_or_else(|| "no monitor is available for the system notification".to_string())?;
    let work_area = monitor.work_area();
    let geometry = system_notification_physical_geometry(
        avatar,
        PhysicalRect {
            x: work_area.position.x,
            y: work_area.position.y,
            width: work_area.size.width,
            height: work_area.size.height,
        },
        monitor.scale_factor(),
        compact,
    );
    let changed = notification.outer_position().ok() != Some(geometry.position)
        || notification.inner_size().ok() != Some(geometry.size);
    set_window_physical_geometry_if_changed(notification, geometry.position, geometry.size)?;
    if changed {
        let placement = if geometry.position.y >= avatar.y + avatar.height as i32 {
            "below"
        } else if geometry.position.x >= avatar.x + avatar.width as i32 {
            "right"
        } else if geometry.position.x + geometry.size.width as i32 <= avatar.x {
            "left"
        } else {
            "above"
        };
        let _ = notification.emit("mascot-system-notification-placement", placement);
    }
    Ok(())
}

fn sync_visible_mascot_system_notification_to_mascot(app: &tauri::AppHandle) {
    let state = app.state::<MascotSystemNotificationState>();
    let Some(compact) = state.visible_compact() else {
        return;
    };
    let (Some(mascot), Some(notification)) = (
        app.get_webview_window("mascot"),
        app.get_webview_window("mascot-notification"),
    ) else {
        return;
    };
    if !matches!(mascot.is_visible(), Ok(true)) || !matches!(notification.is_visible(), Ok(true)) {
        return;
    }
    let _ = position_mascot_system_notification_window(&mascot, &notification, compact);
}

#[tauri::command]
fn set_mascot_system_notification_ready(
    app: tauri::AppHandle,
    state: tauri::State<'_, MascotSystemNotificationState>,
) -> bool {
    let Ok(_transition) = state.transition.lock() else {
        return false;
    };
    let Ok(first_ready) = state.mark_ready() else {
        return false;
    };
    if first_ready {
        if let Some(window) = app.get_webview_window("mascot-notification") {
            let _ = hide_transparent_window_safely(&window);
        }
        state.mark_physical_hidden();
    }
    // Re-announcing renderer readiness must never hide a successfully shown card.
    app.emit_to("mascot", MASCOT_SYSTEM_NOTIFICATION_READY_EVENT, ())
        .is_ok()
}

#[tauri::command]
fn is_mascot_system_notification_ready(
    state: tauri::State<'_, MascotSystemNotificationState>,
) -> bool {
    state.is_ready()
}

#[tauri::command]
fn show_mascot_system_notification_window(
    app: tauri::AppHandle,
    state: tauri::State<'_, MascotSystemNotificationState>,
    compact: Option<bool>,
    client_generation: Option<u64>,
) -> bool {
    let compact = compact.unwrap_or(false);
    let record_result = |success: bool, reason: &str| {
        write_desktop_diagnostic_event(
            &app,
            "notification.native_window_show",
            diagnostic_fields(serde_json::json!({
                "success": success,
                "compact": compact,
                "reason": reason,
                "clientGenerationPresent": client_generation.is_some(),
            })),
        );
        if success && !compact {
            persist_desktop_release_smoke_fields(serde_json::json!({
                "notificationWindowShown": true,
                "notificationCompact": false,
                "notificationProcessId": std::process::id(),
            }));
        }
        success
    };
    let generation = match state.request_show(compact, client_generation) {
        Ok(Some(generation)) => generation,
        Ok(None) => return record_result(false, "not-ready-or-stale"),
        Err(_) => return record_result(false, "state-unavailable"),
    };
    let Ok(_transition) = state.transition.lock() else {
        state.cancel_show(generation);
        return record_result(false, "transition-unavailable");
    };
    if !state.can_show(generation, compact) {
        return record_result(false, "superseded-before-window");
    }
    let Some(mascot) = app.get_webview_window("mascot") else {
        state.cancel_show(generation);
        return record_result(false, "mascot-window-unavailable");
    };
    if !matches!(mascot.is_visible(), Ok(true)) {
        state.cancel_show(generation);
        return record_result(false, "mascot-window-hidden");
    }
    let Some(notification) = app.get_webview_window("mascot-notification") else {
        state.cancel_show(generation);
        return record_result(false, "notification-window-unavailable");
    };
    harden_transparent_window(&notification);
    if matches!(state.visible_compact(), Some(visible_compact) if visible_compact != compact) {
        // Resize only while non-interactive. This avoids exposing a stale
        // transparent WebView2 backbuffer when switching between the short
        // login card and the taller system-message card.
        let _ = hide_transparent_window_safely(&notification);
        state.mark_physical_hidden();
    }
    if position_mascot_system_notification_window(&mascot, &notification, compact).is_err() {
        state.cancel_show(generation);
        let _ = hide_transparent_window_safely(&notification);
        state.mark_physical_hidden();
        return record_result(false, "position-failed");
    }
    if !state.can_show(generation, compact) {
        let _ = hide_transparent_window_safely(&notification);
        state.mark_physical_hidden();
        return record_result(false, "superseded-after-position");
    }
    if !show_interactive_window(&notification, false) {
        state.cancel_show(generation);
        return record_result(false, "native-show-failed");
    }
    if !state.mark_visible(generation, compact) {
        let _ = hide_transparent_window_safely(&notification);
        state.mark_physical_hidden();
        return record_result(false, "superseded-after-show");
    }
    record_result(true, "shown")
}

#[tauri::command]
fn hide_mascot_system_notification_window(
    app: tauri::AppHandle,
    client_generation: Option<u64>,
) -> bool {
    hide_mascot_system_notification_native_window_with_generation(&app, client_generation);
    true
}

#[tauri::command]
fn hide_main_window(
    app: tauri::AppHandle,
    hover_monitor: tauri::State<'_, MascotPeekHoverMonitor>,
) -> bool {
    app.state::<runtime_recovery::DesktopRuntime>().user_hide();
    hover_monitor.cancel();
    // A user-selected hide is different from dismissing the context menu by
    // clicking elsewhere. Publish that intent in the same visibility event so
    // the mascot renderer cannot immediately restore a persistent auth card.
    hide_mascot_context_menu_window_with_restore(&app, false);
    hide_mascot_system_notification_native_window(&app);
    let mascot_hidden = app
        .get_webview_window("mascot")
        .map(|window| hide_transparent_window_safely(&window))
        .unwrap_or(true);
    let panel_hidden = hide_panel_and_notify(&app);
    let notification_hidden = app
        .get_webview_window("mascot-notification")
        .and_then(|window| window.is_visible().ok())
        .map(|visible| !visible)
        .unwrap_or(true);
    write_desktop_diagnostic_event(
        &app,
        "interaction.assistant_hide.completed",
        diagnostic_fields(serde_json::json!({
            "mascotHidden": mascot_hidden,
            "notificationHidden": notification_hidden,
            "panelHidden": panel_hidden,
        })),
    );
    mascot_hidden && notification_hidden
}

#[tauri::command]
fn show_main_window(
    app: tauri::AppHandle,
    motion: tauri::State<'_, MascotDockMotion>,
    initial_placement: tauri::State<'_, InitialMascotPlacement>,
    hover_monitor: tauri::State<'_, MascotPeekHoverMonitor>,
) -> bool {
    let interaction_token = hover_monitor.start();
    hide_mascot_context_menu_window(&app);
    if let Some(window) = app.get_webview_window("mascot") {
        restore_staged_mascot_position(&app, &window);
        ensure_initial_mascot_placement(&window, initial_placement.inner());
        let (width, height) = mascot_logical_size(&window);
        restore_mascot_if_peeked(&window, motion.inner(), width, height);
        if !show_interactive_window(&window, true) {
            return false;
        }
        let _ = app.emit_to("mascot", MASCOT_NATIVE_REVEALED_EVENT, ());
        schedule_mascot_interactivity_recovery(
            window,
            hover_monitor.inner().clone(),
            interaction_token,
        );
        return true;
    }
    false
}

#[tauri::command]
fn show_notification_window(
    app: tauri::AppHandle,
    motion: tauri::State<'_, MascotDockMotion>,
    initial_placement: tauri::State<'_, InitialMascotPlacement>,
    hover_monitor: tauri::State<'_, MascotPeekHoverMonitor>,
) -> bool {
    hover_monitor.cancel();
    hide_mascot_context_menu_window(&app);
    if let Some(window) = app.get_webview_window("mascot") {
        // A reminder should become visible without stealing focus from the
        // document or business application the user is working in.
        restore_staged_mascot_position(&app, &window);
        ensure_initial_mascot_placement(&window, initial_placement.inner());
        let (width, height) = mascot_logical_size(&window);
        restore_mascot_if_peeked(&window, motion.inner(), width, height);
        let _ = app.emit_to("mascot", MASCOT_NATIVE_REVEALED_EVENT, ());
        let shown = show_interactive_window(&window, false);
        write_desktop_diagnostic_event(
            &app,
            "notification.mascot_window_show",
            diagnostic_fields(serde_json::json!({ "success": shown })),
        );
        return shown;
    }
    write_desktop_diagnostic_event(
        &app,
        "notification.mascot_window_show",
        diagnostic_fields(serde_json::json!({
            "success": false,
            "reason": "mascot-window-unavailable",
        })),
    );
    false
}

#[cfg(windows)]
fn schedule_mascot_collapse_recovery(
    app: tauri::AppHandle,
    layout_state: MascotNotificationLayoutState,
    generation: u64,
) {
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(MASCOT_COLLAPSE_RECOVERY_TIMEOUT_MS));
        let Some(window) = app.get_webview_window("mascot") else {
            return;
        };
        let was_visible = matches!(window.is_visible(), Ok(true));
        let recovered = layout_state
            .restore_staged_position_for_generation(&window, generation)
            .unwrap_or(false);
        if !recovered {
            return;
        }

        // The normal path is completed by the renderer after two painted
        // frames. If WebView2 is suspended or the IPC is lost, this native
        // fallback restores hit testing without reopening a mascot the user
        // deliberately hid while the collapse was pending.
        if was_visible {
            let _ = show_interactive_window(&window, false);
            sync_panel_if_visible(&app);
        } else {
            let _ = window.set_ignore_cursor_events(false);
        }
    });
}

#[tauri::command]
fn finish_mascot_notification_collapse(
    app: tauri::AppHandle,
    layout_state: tauri::State<'_, MascotNotificationLayoutState>,
) -> bool {
    let Some(window) = app.get_webview_window("mascot") else {
        return false;
    };
    #[cfg(windows)]
    if layout_state.restore_staged_position(&window).is_err() {
        return false;
    }
    #[cfg(not(windows))]
    let _ = layout_state;
    if !show_interactive_window(&window, false) {
        return false;
    }
    sync_panel_if_visible(&app);
    true
}

fn start_mascot_peek(
    app: &tauri::AppHandle,
    motion: &MascotDockMotion,
    hover_monitor: &MascotPeekHoverMonitor,
    panel_activity: &PanelActivityState,
    reduced_motion: bool,
) -> Option<String> {
    hide_mascot_context_menu_window(app);
    if let Some(window) = app.get_webview_window("mascot") {
        let (width, height) = mascot_logical_size(&window);
        // Expanded reminders and menus must remain fully visible until handled.
        if width > MASCOT_WIDTH + 1.0 || height > MASCOT_HEIGHT + 1.0 {
            return None;
        }
        if let Some(panel) = app.get_webview_window("panel") {
            if matches!(panel.is_visible(), Ok(true)) && panel_activity.is_engaged() {
                return None;
            }
        }
        hide_panel_and_notify(app);
        let side = animate_mascot_dock(
            window.clone(),
            motion.clone(),
            MASCOT_WIDTH,
            MASCOT_HEIGHT,
            true,
            reduced_motion,
        )?;
        let token = hover_monitor.start();
        #[cfg(windows)]
        monitor_mascot_peek_hover(
            app.clone(),
            hover_monitor.clone(),
            motion.clone(),
            token,
            reduced_motion,
        );
        #[cfg(not(windows))]
        let _ = token;
        return Some(side.as_str().to_string());
    }

    None
}

#[tauri::command]
fn peek_mascot_window(
    app: tauri::AppHandle,
    motion: tauri::State<'_, MascotDockMotion>,
    hover_monitor: tauri::State<'_, MascotPeekHoverMonitor>,
    panel_activity: tauri::State<'_, PanelActivityState>,
    reduced_motion: bool,
) -> Option<String> {
    hide_mascot_context_menu_window(&app);
    start_mascot_peek(
        &app,
        motion.inner(),
        hover_monitor.inner(),
        panel_activity.inner(),
        reduced_motion,
    )
}

#[tauri::command]
fn reveal_mascot_window(
    app: tauri::AppHandle,
    motion: tauri::State<'_, MascotDockMotion>,
    hover_monitor: tauri::State<'_, MascotPeekHoverMonitor>,
    reduced_motion: bool,
) {
    hover_monitor.cancel();
    hide_mascot_context_menu_window(&app);
    if let Some(window) = app.get_webview_window("mascot") {
        let (width, height) = mascot_logical_size(&window);
        if !show_interactive_window(&window, false) {
            return;
        }
        animate_mascot_dock(
            window,
            motion.inner().clone(),
            width,
            height,
            false,
            reduced_motion,
        );
    }
}

#[tauri::command]
fn start_mascot_drag(
    app: tauri::AppHandle,
    monitor: tauri::State<'_, MascotDragMonitor>,
    hover_monitor: tauri::State<'_, MascotPeekHoverMonitor>,
) -> Result<(), String> {
    hover_monitor.cancel();
    if !app.state::<PanelActivityState>().has_content() {
        hide_panel_and_notify(&app);
    }
    hide_mascot_context_menu_window(&app);
    let window = app
        .get_webview_window("mascot")
        .ok_or_else(|| "mascot window is unavailable".to_string())?;
    let token = monitor.start();
    #[cfg(windows)]
    monitor_native_drag(app.clone(), monitor.inner().clone(), token);
    #[cfg(not(windows))]
    let _ = (&app, token);

    window.start_dragging().map_err(|error| error.to_string())?;

    Ok(())
}

#[tauri::command]
fn toggle_panel_window(
    app: tauri::AppHandle,
    motion: tauri::State<'_, MascotDockMotion>,
    panel_layout: tauri::State<'_, PanelLayoutState>,
    hover_monitor: tauri::State<'_, MascotPeekHoverMonitor>,
) -> bool {
    hover_monitor.cancel();
    hide_mascot_context_menu_window(&app);
    if let (Some(panel), Some(mascot)) = (
        app.get_webview_window("panel"),
        app.get_webview_window("mascot"),
    ) {
        if matches!(panel.is_visible(), Ok(true)) {
            // Clicking the mascot is an outside click, not explicit dismissal
            // of a draft. Esc and the Hide/Exit commands remain deliberate.
            if app.state::<PanelActivityState>().has_content() {
                return true;
            }
            let _ = hide_transparent_window_safely(&panel);
            emit_panel_visibility(&app, false);
            return false;
        } else {
            let (width, height) = mascot_logical_size(&mascot);
            restore_mascot_if_peeked(&mascot, motion.inner(), width, height);
            if place_panel_near_mascot(&panel, &mascot, panel_layout.height()).is_err() {
                return false;
            }
            if !show_interactive_window(&panel, true) {
                emit_panel_visibility(&app, false);
                return false;
            }
            emit_panel_visibility(&app, true);
            return true;
        }
    }

    false
}

#[tauri::command]
fn show_panel_window(
    app: tauri::AppHandle,
    motion: tauri::State<'_, MascotDockMotion>,
    panel_layout: tauri::State<'_, PanelLayoutState>,
    hover_monitor: tauri::State<'_, MascotPeekHoverMonitor>,
    focus: Option<bool>,
) -> bool {
    hover_monitor.cancel();
    hide_mascot_context_menu_window(&app);
    if let (Some(panel), Some(mascot)) = (
        app.get_webview_window("panel"),
        app.get_webview_window("mascot"),
    ) {
        restore_staged_mascot_position(&app, &mascot);
        let (width, height) = mascot_logical_size(&mascot);
        restore_mascot_if_peeked(&mascot, motion.inner(), width, height);
        // Task pushes use this command as their reminder surface. If the user
        // explicitly hid the assistant, a new task should bring the mascot and
        // its panel back without requiring a tray-menu action.
        let should_focus = focus.unwrap_or(false);
        if place_panel_near_mascot(&panel, &mascot, panel_layout.height()).is_err() {
            return false;
        }
        // Position both hidden windows first. Revealing the mascot before the
        // panel is anchored can expose a stale peek coordinate for one frame.
        if !show_interactive_window(&mascot, false) {
            return false;
        }
        if !show_interactive_window(&panel, should_focus) {
            emit_panel_visibility(&app, false);
            return false;
        }
        emit_panel_visibility(&app, true);
        return true;
    }
    false
}

#[tauri::command]
fn hide_panel_window(app: tauri::AppHandle) -> bool {
    hide_panel_and_notify(&app)
}

#[tauri::command]
fn sync_panel_window(app: tauri::AppHandle) {
    sync_panel_if_visible(&app);
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
fn set_mascot_notification_visible(
    app: tauri::AppHandle,
    motion: tauri::State<'_, MascotDockMotion>,
    layout_state: tauri::State<'_, MascotNotificationLayoutState>,
    initial_placement: tauri::State<'_, InitialMascotPlacement>,
    hover_monitor: tauri::State<'_, MascotPeekHoverMonitor>,
    visible: bool,
    compact: Option<bool>,
    reveal: Option<bool>,
    reduced_motion: Option<bool>,
    hide_during_resize: Option<bool>,
) -> bool {
    hover_monitor.cancel();
    if visible {
        hide_mascot_context_menu_window(&app);
    }
    if let Some(window) = app.get_webview_window("mascot") {
        #[cfg(windows)]
        if visible && layout_state.restore_staged_position(&window).is_err() {
            return false;
        }
        // The first frontend layout request can race the hidden window's native
        // setup on Windows. Anchor the collapsed mascot before calculating the
        // expanded login/reminder bounds so no stale top-left restore position
        // can pull the card back or leave it clipped to a thin border.
        ensure_initial_mascot_placement(&window, initial_placement.inner());
        // Long-lived login and system-message cards are isolated in
        // `mascot-notification`. The mascot HWND may expand only to the compact
        // bubble size; never recreate the old 320x480 transparent hit-test mask
        // even if a stale renderer sends the former expanded-layout argument.
        let _ = compact;
        let compact = visible;
        #[cfg(not(windows))]
        let _ = hide_during_resize;
        #[cfg(windows)]
        let suspended_for_resize = !visible && hide_during_resize.unwrap_or(false);
        #[cfg(not(windows))]
        let suspended_for_resize = false;
        if suspended_for_resize && !hide_transparent_window_safely(&window) {
            return false;
        }
        if resize_mascot_for_notification(
            &window,
            motion.inner(),
            layout_state.inner(),
            visible,
            compact,
            reveal.unwrap_or(false),
            reduced_motion.unwrap_or(false),
        )
        .is_err()
        {
            // Best-effort rollback prevents a failed expansion from leaving a
            // partially resized transparent HWND behind the avatar.
            let _ = resize_mascot_for_notification(
                &window,
                motion.inner(),
                layout_state.inner(),
                false,
                false,
                false,
                true,
            );
            if suspended_for_resize {
                let _ = show_interactive_window(&window, false);
            }
            return false;
        }
        #[cfg(windows)]
        if suspended_for_resize {
            let target_position = match window.outer_position() {
                Ok(position) => position,
                Err(_) => {
                    let _ = show_interactive_window(&window, false);
                    return false;
                }
            };
            let recovery_generation = match layout_state.stage_collapsed_position(target_position) {
                Ok(generation) => generation,
                Err(_) => {
                    let _ = show_interactive_window(&window, false);
                    return false;
                }
            };
            let staged = window
                .set_position(Position::Physical(PhysicalPosition::new(-32_000, -32_000)))
                .is_ok()
                && show_window_without_activation(&window).is_ok();
            if !staged {
                let _ = layout_state.restore_staged_position(&window);
                let _ = show_interactive_window(&window, false);
                return false;
            }
            schedule_mascot_collapse_recovery(
                app.clone(),
                layout_state.inner().clone(),
                recovery_generation,
            );
        }
        if !visible && !suspended_for_resize {
            // A panel may be opened while a bubble is fading. Re-anchor it only
            // after the mascot has atomically returned to collapsed bounds.
            sync_panel_if_visible(&app);
        }
        if !suspended_for_resize {
            sync_visible_mascot_system_notification_to_mascot(&app);
        }
        return true;
    }
    false
}

#[tauri::command]
fn set_panel_height(
    app: tauri::AppHandle,
    panel_layout: tauri::State<'_, PanelLayoutState>,
    height: f64,
) {
    panel_layout.set_height(height);
    if let (Some(panel), Some(mascot)) = (
        app.get_webview_window("panel"),
        app.get_webview_window("mascot"),
    ) {
        let _ = place_panel_near_mascot(&panel, &mascot, height);
    }
}

#[tauri::command]
fn set_panel_activity(state: tauri::State<'_, PanelActivityState>, has_text: bool, focused: bool) {
    state.set(has_text, focused);
}

#[tauri::command]
fn exit_app(app: tauri::AppHandle) {
    hide_mascot_context_menu_window(&app);
    hide_mascot_system_notification_native_window(&app);
    app.exit(0);
}

fn is_http_url(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

#[cfg(windows)]
fn record_visual_smoke_workbench_open(url: &str) -> bool {
    if !matches!(
        std::env::var("HUALI_AI_VISUAL_SMOKE_FORCE_MOTION").as_deref(),
        Ok("1")
    ) || !url
        .split('?')
        .next()
        .is_some_and(|path| path.ends_with("/workbench"))
    {
        return false;
    }

    // The interaction gate needs proof that the real pointer sequence reached
    // the workbench command, but it must never persist the URL or user fields.
    let receipt = serde_json::json!({ "workbenchOpened": true });
    let path = std::env::temp_dir().join(format!(
        "huali-ai-workbench-smoke-{}.json",
        std::process::id()
    ));
    serde_json::to_vec(&receipt)
        .ok()
        .is_some_and(|serialized| fs::write(path, serialized).is_ok())
}

#[cfg(target_os = "macos")]
fn focus_existing_browser_tab(url: &str, match_url: &str) -> bool {
    let script = r##"
on run argv
  set targetUrl to item 1 of argv
  set matchUrl to item 2 of argv

  if my focusChromium("Google Chrome", targetUrl, matchUrl) then return "reused"
  if my focusChromium("Microsoft Edge", targetUrl, matchUrl) then return "reused"
  if my focusChromium("Brave Browser", targetUrl, matchUrl) then return "reused"
  if my focusChromium("Arc", targetUrl, matchUrl) then return "reused"
  if my focusSafari(targetUrl, matchUrl) then return "reused"

  return "not_found"
end run

on isMatchingUrl(currentUrl, matchUrl)
  if currentUrl is missing value then return false
  if currentUrl is equal to matchUrl then return true
  if currentUrl starts with (matchUrl & "/") then return true
  if currentUrl starts with (matchUrl & "?") then return true
  if currentUrl starts with (matchUrl & "#") then return true
  return false
end isMatchingUrl

on focusChromium(browserName, targetUrl, matchUrl)
  try
    if application browserName is not running then return false
    using terms from application "Google Chrome"
      tell application browserName
        repeat with browserWindow in windows
          set tabIndex to 1
          repeat with browserTab in tabs of browserWindow
            set currentUrl to URL of browserTab
            if my isMatchingUrl(currentUrl, matchUrl) then
              set active tab index of browserWindow to tabIndex
              set index of browserWindow to 1
              set URL of browserTab to targetUrl
              activate
              return true
            end if
            set tabIndex to tabIndex + 1
          end repeat
        end repeat
      end tell
    end using terms from
  end try
  return false
end focusChromium

on focusSafari(targetUrl, matchUrl)
  try
    if application "Safari" is not running then return false
    tell application "Safari"
      repeat with browserWindow in windows
        repeat with browserTab in tabs of browserWindow
          set currentUrl to URL of browserTab
          if my isMatchingUrl(currentUrl, matchUrl) then
            set current tab of browserWindow to browserTab
            set index of browserWindow to 1
            set URL of browserTab to targetUrl
            activate
            return true
          end if
        end repeat
      end repeat
    end tell
  end try
  return false
end focusSafari
"##;

    let Ok(output) = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .arg(url)
        .arg(match_url)
        .output()
    else {
        return false;
    };

    output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "reused"
}

#[cfg(target_os = "windows")]
fn focus_existing_browser_tab(url: &str, match_url: &str) -> bool {
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    let script = r##"
param([string]$TargetUrl, [string]$MatchUrl)

$ErrorActionPreference = 'SilentlyContinue'
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -AssemblyName System.Windows.Forms
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class HualiBrowserWindow {
  [DllImport("user32.dll")]
  public static extern bool SetForegroundWindow(IntPtr hWnd);
}
'@

function Test-HualiUrlMatch([string]$CurrentUrl, [string]$BaseUrl) {
  if ([string]::IsNullOrWhiteSpace($CurrentUrl)) { return $false }
  $current = $CurrentUrl.TrimEnd('/')
  $base = $BaseUrl.TrimEnd('/')
  return $current -eq $base -or
    $current.StartsWith($base + '/') -or
    $current.StartsWith($base + '?') -or
    $current.StartsWith($base + '#')
}

function Get-HualiAddressBar($Root) {
  $condition = New-Object System.Windows.Automation.PropertyCondition(
    [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
    [System.Windows.Automation.ControlType]::Edit
  )
  $edits = $Root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $condition)
  foreach ($edit in $edits) {
    try {
      $pattern = $edit.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern)
      $value = $pattern.Current.Value
      if ($value -match '^https?://') {
        return [PSCustomObject]@{ Element = $edit; Pattern = $pattern; Value = $value }
      }
    } catch {}
  }
  return $null
}

$processes = Get-Process -Name msedge, chrome, brave, firefox -ErrorAction SilentlyContinue |
  Where-Object { $_.MainWindowHandle -ne 0 } |
  Sort-Object Id -Unique

foreach ($process in $processes) {
  try {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($process.MainWindowHandle)
    if ($null -eq $root) { continue }

    $tabCondition = New-Object System.Windows.Automation.PropertyCondition(
      [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
      [System.Windows.Automation.ControlType]::TabItem
    )
    $tabs = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $tabCondition)
    $originalTab = $null

    foreach ($tab in $tabs) {
      try {
        $selection = $tab.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern)
        if ($selection.Current.IsSelected) { $originalTab = $selection }
        $selection.Select()
        Start-Sleep -Milliseconds 70
        $addressBar = Get-HualiAddressBar $root
        if ($null -ne $addressBar -and (Test-HualiUrlMatch $addressBar.Value $MatchUrl)) {
          [HualiBrowserWindow]::SetForegroundWindow($process.MainWindowHandle) | Out-Null
          $addressBar.Pattern.SetValue($TargetUrl)
          $addressBar.Element.SetFocus()
          [System.Windows.Forms.SendKeys]::SendWait('{ENTER}')
          Write-Output 'reused'
          exit 0
        }
      } catch {}
    }

    if ($null -ne $originalTab) { $originalTab.Select() }

    if ($tabs.Count -eq 0) {
      $addressBar = Get-HualiAddressBar $root
      if ($null -ne $addressBar -and (Test-HualiUrlMatch $addressBar.Value $MatchUrl)) {
        [HualiBrowserWindow]::SetForegroundWindow($process.MainWindowHandle) | Out-Null
        $addressBar.Pattern.SetValue($TargetUrl)
        $addressBar.Element.SetFocus()
        [System.Windows.Forms.SendKeys]::SendWait('{ENTER}')
        Write-Output 'reused'
        exit 0
      }
    }
  } catch {}
}

Write-Output 'not_found'
"##;

    let Ok(output) = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
            url,
            match_url,
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    else {
        return false;
    };

    output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "reused"
}

#[cfg(not(any(target_os = "macos", windows)))]
fn focus_existing_browser_tab(_url: &str, _match_url: &str) -> bool {
    false
}

#[tauri::command]
async fn open_or_focus_web_url(url: String, match_url: String) -> bool {
    if !is_http_url(&url) || !is_http_url(&match_url) {
        return false;
    }

    #[cfg(windows)]
    if record_visual_smoke_workbench_open(&url) {
        return true;
    }

    tauri::async_runtime::spawn_blocking(move || focus_existing_browser_tab(&url, &match_url))
        .await
        .unwrap_or(false)
}

#[cfg_attr(not(windows), allow(dead_code))]
fn reset_window_for_runtime_recovery(app: &tauri::AppHandle, label: &str) {
    if label == "mascot" {
        app.state::<MascotPeekHoverMonitor>().cancel();
        app.state::<MascotDockMotion>().cancel();
        hide_mascot_context_menu_window(app);
        hide_mascot_system_notification_native_window(app);
    } else if label == "mascot-notification" {
        let state = app.state::<MascotSystemNotificationState>();
        if let Ok(mut status) = state.status.lock() {
            let generation = status.generation.wrapping_add(1);
            let client_generation = status.client_generation;
            *status = MascotSystemNotificationStatus {
                generation,
                client_generation,
                ..Default::default()
            };
        };
    } else if label == "mascot-menu" {
        let state = app.state::<MascotContextMenuState>();
        if let Ok(mut status) = state.status.lock() {
            let generation = status.generation.wrapping_add(1);
            *status = MascotContextMenuStatus {
                generation,
                ..Default::default()
            };
        };
        let _ = emit_mascot_context_menu_visibility(app, false, true);
    }
}

fn configure_desktop_window(
    app: &tauri::AppHandle,
    window: &tauri::WebviewWindow,
) -> tauri::Result<()> {
    if window.label() == "mascot" {
        harden_transparent_window(window);
        window.set_ignore_cursor_events(true)?;
        let _ = place_mascot_bottom_right(window);
        let close_window = window.clone();
        let close_app = app.clone();
        window.on_window_event(move |event| match event {
            tauri::WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                close_app.state::<MascotPeekHoverMonitor>().cancel();
                hide_mascot_context_menu_window_with_restore(&close_app, false);
                hide_mascot_system_notification_native_window(&close_app);
                let _ = hide_transparent_window_safely(&close_window);
                hide_panel_and_notify(&close_app);
            }
            tauri::WindowEvent::Moved(_) => {
                // Windows native dragging has one compositor-paced
                // monitor above. Other platforms use their move event.
                #[cfg(not(windows))]
                sync_visible_mascot_system_notification_to_mascot(&close_app);
            }
            _ => {}
        });
    }
    if window.label() == "panel" {
        harden_transparent_window(window);
        window.set_ignore_cursor_events(true)?;
        let close_app = app.clone();
        let app_handle = app.clone();
        window.on_window_event(move |event| match event {
            tauri::WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                hide_panel_and_notify(&close_app);
            }
            tauri::WindowEvent::Focused(false) => {
                hide_panel_after_focus_moves_outside_app(app_handle.clone());
            }
            _ => {}
        });
    }
    if window.label() == "mascot-menu" {
        harden_transparent_window(window);
        let close_app = app.clone();
        let app_handle = app.clone();
        window.on_window_event(move |event| match event {
            tauri::WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                hide_mascot_context_menu_window(&close_app);
            }
            tauri::WindowEvent::Focused(false) => {
                hide_context_menu_after_focus_moves_outside_app(app_handle.clone());
            }
            _ => {}
        });
        #[cfg(windows)]
        {
            // A WebView2 hosted by a never-visible HWND may postpone
            // navigation indefinitely. Warm it up fully transparent,
            // click-through and off-screen; its ready IPC immediately
            // hides it before any user interaction can occur.
            window.set_ignore_cursor_events(true)?;
            window.set_position(Position::Physical(PhysicalPosition::new(-32_000, -32_000)))?;
            show_window_without_activation(window).map_err(std::io::Error::other)?;
        }
    }
    if window.label() == "mascot-notification" {
        harden_transparent_window(window);
        let close_app = app.clone();
        window.on_window_event(move |event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                hide_mascot_system_notification_native_window(&close_app);
            }
        });
        #[cfg(windows)]
        {
            // Keep this renderer active before the first system message,
            // but never expose its warm-up surface on the desktop.
            window.set_ignore_cursor_events(true)?;
            window.set_position(Position::Physical(PhysicalPosition::new(-32_000, -32_000)))?;
            show_window_without_activation(window).map_err(std::io::Error::other)?;
        }
    }

    #[cfg(windows)]
    windows_session::install_process_handler(window);
    Ok(())
}

fn main() {
    let startup_args = std::env::args().collect::<Vec<_>>();
    if let Some(callback_url) = find_desktop_auth_callback(&startup_args) {
        persist_startup_desktop_auth_callback(&callback_url);
        persist_desktop_auth_smoke_receipt(&callback_url, None, None);
    }

    // Windows Server runners commonly expose the OS reduced-motion preference
    // to WebView2. Keep the product's accessibility behavior unchanged, while
    // allowing the isolated visual release gate to exercise real sprite
    // progression through WebView2's programmatic environment options. Using
    // Tauri's context is important here: elevated WebView2 hosts can ignore the
    // similarly named machine/process browser-argument environment override.
    #[cfg(windows)]
    let visual_smoke_force_motion = matches!(
        std::env::var("HUALI_AI_VISUAL_SMOKE_FORCE_MOTION").as_deref(),
        Ok("1")
    );
    #[cfg(not(windows))]
    let visual_smoke_force_motion = false;
    #[cfg(windows)]
    let context = {
        let mut context = tauri::generate_context!();
        if visual_smoke_force_motion {
            const VISUAL_SMOKE_BROWSER_ARGS: &str =
                "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection \
                 --autoplay-policy=no-user-gesture-required \
                 --force-prefers-no-reduced-motion";

            for window in &mut context.config_mut().app.windows {
                // Tauri's config accepts only a relative data_directory and
                // resolves it below each window label. Build these three windows
                // in setup instead, where WebviewWindowBuilder can apply one
                // shared absolute UDF to every WebView2 environment.
                window.create = false;
                window.additional_browser_args = Some(VISUAL_SMOKE_BROWSER_ARGS.to_owned());
            }
        }
        context
    };
    #[cfg(not(windows))]
    let context = tauri::generate_context!();

    let pending_desktop_auth = PendingDesktopAuthCallback::default();
    let single_instance_desktop_auth = pending_desktop_auth.clone();

    tauri::Builder::default()
        .manage(pending_desktop_auth)
        .plugin(tauri_plugin_single_instance::init(
            move |app, argv, _cwd| {
                if let Some(callback_url) = single_instance_desktop_auth.capture(&argv) {
                    let mut fields = desktop_auth_callback_diagnostic_fields(&callback_url);
                    fields.extend(diagnostic_fields(serde_json::json!({
                        "argumentCount": argv.len(),
                        "argumentsJson": serde_json::to_string(&argv).unwrap_or_default(),
                    })));
                    write_desktop_diagnostic_event(
                        app,
                        "auth.callback.single_instance_received",
                        fields,
                    );
                    // Receipt is not authentication. Keep the login card visible
                    // until Vue validates state + identity and commits the new
                    // session; otherwise a malformed callback looks successful
                    // for two minutes before the waiting card reappears.
                    persist_desktop_auth_smoke_receipt(&callback_url, Some(true), None);
                    let _ = app.emit("desktop-auth-callback", callback_url);
                }

                if visual_smoke_force_motion
                    && argv.iter().any(|arg| arg == VISUAL_SMOKE_PEEK_ARGUMENT)
                {
                    let motion = app.state::<MascotDockMotion>();
                    let hover_monitor = app.state::<MascotPeekHoverMonitor>();
                    let panel_activity = app.state::<PanelActivityState>();
                    let _ = start_mascot_peek(
                        app,
                        motion.inner(),
                        hover_monitor.inner(),
                        panel_activity.inner(),
                        false,
                    );
                    return;
                }

                if let Some(window) = app.get_webview_window("mascot") {
                    app.state::<MascotPeekHoverMonitor>().cancel();
                    restore_staged_mascot_position(app, &window);
                    let initial_placement = app.state::<InitialMascotPlacement>();
                    ensure_initial_mascot_placement(&window, initial_placement.inner());
                    let motion = app.state::<MascotDockMotion>();
                    let (width, height) = mascot_logical_size(&window);
                    restore_mascot_if_peeked(&window, motion.inner(), width, height);
                    let _ = app.emit_to("mascot", MASCOT_NATIVE_REVEALED_EVENT, ());
                    let _ = show_interactive_window(&window, true);
                }
            },
        ))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_http::init())
        .plugin(tauri_plugin_opener::init())
        .manage(runtime_recovery::DesktopRuntime::default())
        .manage(InitialMascotPlacement::default())
        .manage(MascotDockMotion::default())
        .manage(MascotSystemNotificationState::default())
        .manage(mascot_notification_layout_state())
        .manage(MascotContextMenuState::default())
        .manage(MascotDragMonitor::default())
        .manage(MascotPeekHoverMonitor::default())
        .manage(PanelActivityState::default())
        .manage(PanelLayoutState::default())
        .invoke_handler(tauri::generate_handler![
            runtime_recovery::desktop_runtime_ready,
            runtime_recovery::desktop_runtime_mounted,
            runtime_recovery::desktop_runtime_ack,
            runtime_recovery::request_notification_recovery,
            hide_main_window,
            show_mascot_context_menu,
            ack_mascot_context_menu_layout,
            hide_mascot_context_menu,
            set_mascot_context_menu_ready,
            set_mascot_system_notification_ready,
            is_mascot_system_notification_ready,
            show_mascot_system_notification_window,
            hide_mascot_system_notification_window,
            show_main_window,
            show_notification_window,
            finish_mascot_notification_collapse,
            peek_mascot_window,
            reveal_mascot_window,
            start_mascot_drag,
            toggle_panel_window,
            show_panel_window,
            hide_panel_window,
            sync_panel_window,
            set_mascot_notification_visible,
            set_panel_height,
            set_panel_activity,
            exit_app,
            open_or_focus_web_url,
            record_desktop_diagnostic_event,
            get_desktop_diagnostic_log_path,
            record_desktop_auth_renderer_receipt,
            take_desktop_auth_callback,
            get_desktop_release_smoke_config,
            record_desktop_release_smoke_session,
            record_desktop_release_smoke_restart
        ])
        .setup(move |app| {
            // v1.0.51 was a temporary P0 diagnostic build. Formal releases
            // remove only its two known per-user JSONL files when present;
            // the log directory and all other user configuration are retained.
            cleanup_desktop_diagnostic_logs(app.handle());

            #[cfg(windows)]
            if visual_smoke_force_motion {
                let visual_smoke_data_directory = std::env::temp_dir()
                    .join(format!("huali-ai-visual-smoke-{}", std::process::id()));
                let window_configs = app.config().app.windows.clone();
                for window_config in window_configs {
                    tauri::WebviewWindowBuilder::from_config(app.handle(), &window_config)?
                        .data_directory(visual_smoke_data_directory.clone())
                        .build()?;
                }
            }

            let startup_args = std::env::args().collect::<Vec<_>>();
            app.state::<PendingDesktopAuthCallback>()
                .capture(&startup_args);
            let startup_callback = find_desktop_auth_callback(&startup_args);
            let mut process_start_fields = diagnostic_fields(serde_json::json!({
                "argumentCount": startup_args.len(),
                "argumentsJson": serde_json::to_string(&startup_args).unwrap_or_default(),
                "startupCallbackPresent": startup_callback.is_some(),
                "releaseSmoke": desktop_release_smoke_nonce().is_some(),
            }));
            if let Some(callback_url) = startup_callback.as_deref() {
                process_start_fields.extend(desktop_auth_callback_diagnostic_fields(callback_url));
            }
            write_desktop_diagnostic_event(app.handle(), "process.start", process_start_fields);

            #[cfg(any(windows, target_os = "linux"))]
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                if let Err(error) = app.deep_link().register_all() {
                    eprintln!("deep link register failed: {error}");
                }
            }

            for label in runtime_health::LABELS {
                if let Some(window) = app.get_webview_window(label) {
                    configure_desktop_window(app.handle(), &window)?;
                }
            }
            runtime_recovery::initialize(app.handle());

            let open = MenuItem::with_id(app, "open_workbench", "打开工作台", true, None::<&str>)?;
            let show = MenuItem::with_id(app, "show", "显示助手", true, None::<&str>)?;
            let hide = MenuItem::with_id(app, "hide", "隐藏助手", true, None::<&str>)?;
            let logout = MenuItem::with_id(app, "logout", "退出登录", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &show, &hide, &logout, &quit])?;

            // Embed a dedicated small icon instead of relying on the optional
            // default window icon. This keeps the Windows notification-area
            // icon visible even when a platform does not expose a default.
            let tray_icon = tauri::image::Image::from_bytes(include_bytes!("../icons/32x32.png"))?;

            TrayIconBuilder::new()
                .icon(tray_icon)
                .tooltip("华力 AI 桌面助手")
                .menu(&menu)
                .show_menu_on_left_click(true)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "open_workbench" => {
                        hide_mascot_context_menu_window(app);
                        let _ = app.emit("tray-open-workbench", ());
                    }
                    "show" => {
                        hide_mascot_context_menu_window(app);
                        app.state::<MascotPeekHoverMonitor>().cancel();
                        if let Some(window) = app.get_webview_window("mascot") {
                            restore_staged_mascot_position(app, &window);
                            let initial_placement = app.state::<InitialMascotPlacement>();
                            ensure_initial_mascot_placement(&window, initial_placement.inner());
                            let motion = app.state::<MascotDockMotion>();
                            let (width, height) = mascot_logical_size(&window);
                            restore_mascot_if_peeked(&window, motion.inner(), width, height);
                            let _ = app.emit_to("mascot", MASCOT_NATIVE_REVEALED_EVENT, ());
                            let _ = show_interactive_window(&window, true);
                        }
                    }
                    "hide" => {
                        app.state::<runtime_recovery::DesktopRuntime>().user_hide();
                        hide_mascot_context_menu_window_with_restore(app, false);
                        app.state::<MascotPeekHoverMonitor>().cancel();
                        hide_mascot_system_notification_native_window(app);
                        if let Some(window) = app.get_webview_window("mascot") {
                            let _ = hide_transparent_window_safely(&window);
                        }
                        hide_panel_and_notify(app);
                    }
                    "logout" => {
                        hide_mascot_context_menu_window(app);
                        let _ = app.emit("tray-logout", ());
                    }
                    "quit" => {
                        hide_mascot_context_menu_window(app);
                        app.exit(0);
                    }
                    _ => {}
                })
                .build(app)?;

            Ok(())
        })
        .build(context)
        .expect("error while building huali ai mascot")
        .run(|app, event| {
            // Recreating the last WebView must not exit the tray host. An
            // explicit user quit (Some(0)) still exits immediately.
            if let tauri::RunEvent::ExitRequested {
                code: None, api, ..
            } = event
            {
                if app.state::<runtime_recovery::DesktopRuntime>().rebuilding() {
                    api.prevent_exit();
                }
            }
        });
}
