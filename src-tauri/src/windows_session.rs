//! An invisible native observer on its own message-loop thread. It continues
//! receiving session/power events when a WebView renderer is blocked.
use std::time::Instant;
use tauri::Manager;
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    System::{LibraryLoader::GetModuleHandleW, RemoteDesktop::*},
    UI::WindowsAndMessaging::*,
};

struct Observer {
    app: tauri::AppHandle,
    last_tick: Instant,
    suspended: bool,
}

fn interactive_session() -> Option<bool> {
    let mut buffer = std::ptr::null_mut();
    let mut bytes = 0;
    unsafe {
        if WTSQuerySessionInformationW(
            std::ptr::null_mut(),
            WTS_CURRENT_SESSION,
            WTSSessionInfoEx,
            &mut buffer,
            &mut bytes,
        ) == 0
        {
            return None;
        }
        let result = if bytes as usize >= std::mem::size_of::<WTSINFOEXW>() {
            let info = &*(buffer as *const WTSINFOEXW);
            if info.Level == 1 {
                let data = info.Data.WTSInfoExLevel1;
                match data.SessionFlags as u32 {
                    WTS_SESSIONSTATE_LOCK => Some(false),
                    WTS_SESSIONSTATE_UNLOCK => Some(data.SessionState == WTSActive),
                    _ => None,
                }
            } else {
                None
            }
        } else {
            None
        };
        WTSFreeMemory(buffer.cast());
        result
    }
}

unsafe extern "system" fn observer_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_NCCREATE {
        let create = &*(lparam as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
    }
    let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Observer;
    if !pointer.is_null() {
        let observer = &mut *pointer;
        match msg {
            WM_WTSSESSION_CHANGE => {
                let interactive = match wparam as u32 {
                    WTS_SESSION_LOCK | WTS_CONSOLE_DISCONNECT | WTS_REMOTE_DISCONNECT => {
                        Some(false)
                    }
                    WTS_SESSION_UNLOCK | WTS_CONSOLE_CONNECT | WTS_REMOTE_CONNECT => {
                        interactive_session()
                    }
                    _ => None,
                };
                if let Some(active) = interactive {
                    crate::runtime_recovery::session_changed(
                        &observer.app,
                        active && !observer.suspended,
                        false,
                    );
                }
            }
            WM_POWERBROADCAST => match wparam as u32 {
                PBT_APMSUSPEND => {
                    observer.suspended = true;
                    crate::runtime_recovery::session_changed(&observer.app, false, false);
                }
                PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND => {
                    observer.suspended = false;
                    crate::runtime_recovery::session_changed(
                        &observer.app,
                        interactive_session().unwrap_or(false),
                        true,
                    );
                    observer.last_tick = Instant::now();
                }
                _ => {}
            },
            WM_TIMER => {
                let delayed = observer.last_tick.elapsed().as_secs() > 30;
                observer.last_tick = Instant::now();
                if let Some(active) = interactive_session() {
                    crate::runtime_recovery::session_changed(
                        &observer.app,
                        active && !observer.suspended,
                        delayed,
                    );
                }
                crate::runtime_recovery::schedule_tick(&observer.app);
            }
            WM_NCDESTROY => {
                WTSUnRegisterSessionNotification(hwnd);
                KillTimer(hwnd, 1);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
            _ => {}
        }
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

pub fn start(app: tauri::AppHandle) {
    std::thread::spawn(move || unsafe {
        let name: Vec<u16> = "HualiRuntimeSessionObserver\0".encode_utf16().collect();
        let module = GetModuleHandleW(std::ptr::null());
        let class = WNDCLASSW {
            lpfnWndProc: Some(observer_proc),
            hInstance: module,
            lpszClassName: name.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassW(&class);
        if let Some(active) = interactive_session() {
            crate::runtime_recovery::session_changed(&app, active, false);
        }
        let mut observer = Box::new(Observer {
            app,
            last_tick: Instant::now(),
            suspended: false,
        });
        // A hidden top-level HWND receives power broadcasts, unlike HWND_MESSAGE.
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            name.as_ptr(),
            name.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            module,
            (observer.as_mut() as *mut Observer).cast(),
        );
        if hwnd.is_null() {
            return;
        }
        WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION);
        SetTimer(hwnd, 1, 5_000, None);
        let mut message = std::mem::zeroed();
        while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        DestroyWindow(hwnd);
    });
}

pub fn install_process_handler(window: &tauri::WebviewWindow) {
    use webview2_com::{Microsoft::Web::WebView2::Win32::*, ProcessFailedEventHandler};
    let app = window.app_handle().clone();
    let label = window.label().to_string();
    let generation = crate::runtime_recovery::register_native_generation(&app, &label);
    let _ = window.with_webview(move |webview| unsafe {
        let Ok(core) = webview.controller().CoreWebView2() else {
            return;
        };
        let handler = ProcessFailedEventHandler::create(Box::new(move |_, args| {
            if let Some(args) = args {
                let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND(0);
                if args.ProcessFailedKind(&mut kind).is_ok() {
                    if kind == COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED
                        || kind == COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED
                        || kind == COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_UNRESPONSIVE
                    {
                        crate::runtime_recovery::process_failed(
                            &app,
                            &label,
                            kind == COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED,
                            generation,
                        );
                    }
                }
            }
            Ok(())
        }));
        let mut token = 0;
        let _ = core.add_ProcessFailed(&handler, &mut token);
    });
}
