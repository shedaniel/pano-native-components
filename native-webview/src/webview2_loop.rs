use std::cell::RefCell;
use std::ffi::c_void;
use std::rc::Rc;
use std::sync::Mutex;

use windows_webview::{Controller, Environment, EnvironmentOptions, EventRegistration, WebView};
use windows_window::Window;

use crate::webview_event::{WebViewIncomingEvent, WebViewOutgoingEvent};

#[link(name = "user32")]
unsafe extern "system" {
    // JNI calls on arbitrary JVM threads -> UI thread
    fn PostMessageW(hwnd: *mut c_void, msg: u32, wparam: usize, lparam: isize) -> i32;
}

const WM_APP_USER_EVENT: u32 = 0x8000; // WM_APP
const WM_CLOSE: u32 = 0x0010;

static DISPATCHER_HWND: Mutex<Option<isize>> = Mutex::new(None);
static WEBVIEW_INITIAL_EVENT: Mutex<Option<WebViewIncomingEvent>> = Mutex::new(None);

pub fn send_incoming_webview_event(event: WebViewIncomingEvent) {
    let hwnd = *DISPATCHER_HWND.lock().unwrap();
    if let Some(hwnd) = hwnd {
        let boxed = Box::into_raw(Box::new(event));
        let ok = unsafe { PostMessageW(hwnd as *mut c_void, WM_APP_USER_EVENT, 0, boxed as isize) };
        if ok == 0 {
            eprintln!("Error posting message to event loop");
            unsafe { drop(Box::from_raw(boxed)) };
        }
    } else {
        *WEBVIEW_INITIAL_EVENT.lock().unwrap() = Some(event);
        eprintln!("Event loop not running");
    }
}

/// Everything tied to the current webview. Dropping them tears it down
struct SessionState {
    controller: Controller,
    webview: WebView,
    _registration: EventRegistration,
}

pub fn event_loop(jni_callback: impl Fn(WebViewOutgoingEvent) + 'static) {
    let jni_callback: Rc<dyn Fn(WebViewOutgoingEvent)> = Rc::new(jni_callback);
    let state = Rc::new(RefCell::new(Option::<SessionState>::None));

    let window_cell: Rc<RefCell<Option<Window>>> = Rc::new(RefCell::new(None));

    let window = Window::new("WebView")
        .on_resize({
            let state = state.clone();
            move |width, height| {
                if let Some(state) = &*state.borrow() {
                    let _ = state.controller.set_bounds(0, 0, width, height);
                }
            }
        })
        .on_message({
            let state = state.clone();
            let jni_callback = jni_callback.clone();
            let window_cell = window_cell.clone();
            move |_hwnd, message, _wparam, lparam| match message {
                WM_APP_USER_EVENT => {
                    let event = unsafe { *Box::from_raw(lparam as *mut WebViewIncomingEvent) };
                    let window_ref = window_cell.borrow();
                    let window = window_ref
                        .as_ref()
                        .expect("dispatcher window not yet stored");
                    handle_event(event, window, &state, &jni_callback);
                    Some(0)
                }
                WM_CLOSE => {
                    // the same as WebViewIncomingEvent::Close
                    state.take();
                    None
                }
                _ => None,
            }
        })
        .create()
        .expect("failed to create host window");

    let hwnd = window.hwnd();
    *window_cell.borrow_mut() = Some(window);
    *DISPATCHER_HWND.lock().unwrap() = Some(hwnd as isize);

    if let Some(event) = WEBVIEW_INITIAL_EVENT.lock().unwrap().take() {
        let window_ref = window_cell.borrow();
        handle_event(event, window_ref.as_ref().unwrap(), &state, &jni_callback);
    }

    windows_window::run();

    // Session over: clear the dispatcher HWND so a later startEventLoop call
    // (for the next login) is detected as "not running" until it re-runs.
    *DISPATCHER_HWND.lock().unwrap() = None;
}

fn handle_event(
    event: WebViewIncomingEvent,
    window: &Window,
    state: &Rc<RefCell<Option<SessionState>>>,
    jni_callback: &Rc<dyn Fn(WebViewOutgoingEvent)>,
) {
    match event {
        WebViewIncomingEvent::LaunchWebView(
            url,
            callback_prefix,
            cookies_url,
            data_dir,
            proxy_type,
            proxy_host,
            proxy_port,
        ) => {
            let mut options = EnvironmentOptions::new().user_data_folder(&data_dir);
            if !proxy_type.is_empty() && !proxy_host.is_empty() && proxy_port != 0 {
                options = options.additional_browser_arguments(format!(
                    "--proxy-server={proxy_type}://{proxy_host}:{proxy_port}"
                ));
            }

            let environment = match Environment::with_options(&options) {
                Ok(e) => e,
                Err(e) => return eprintln!("Failed to create WebView2 environment: {e}"),
            };
            let controller = match environment.create_controller(window) {
                Ok(c) => c,
                Err(e) => return eprintln!("Failed to create WebView2 controller: {e}"),
            };
            let webview = match controller.webview() {
                Ok(w) => w,
                Err(e) => return eprintln!("Failed to get WebView2 webview: {e}"),
            };

            let registration = {
                let cookies_url = cookies_url.clone();
                match webview.on_navigation_starting(move |args| {
                    let url = args.uri();
                    if url.starts_with(&callback_prefix) {
                        let _ = args.set_cancel(true);
                        send_incoming_webview_event(WebViewIncomingEvent::SendCallback(
                            url,
                            cookies_url.clone(),
                        ));
                    }
                }) {
                    Ok(reg) => reg,
                    Err(e) => return eprintln!("Failed to subscribe to navigation events: {e}"),
                }
            };

            if let Err(e) = webview.navigate(&url) {
                eprintln!("Failed to navigate: {e}");
            }

            // Match the window to the webview's initial bounds
            let (width, height) = window.client_size();
            let _ = controller.set_bounds(0, 0, width, height);

            state.replace(Some(SessionState {
                controller,
                webview,
                _registration: registration,
            }));
        }

        WebViewIncomingEvent::SendCallback(url, cookies_url) => {
            let state_ref = state.borrow();
            if let Some(SessionState { webview, .. }) = &*state_ref {
                let jni_callback = jni_callback.clone();
                let cookie_manager = webview.cookie_manager().unwrap();
                cookie_manager
                    .get_cookies(&cookies_url, move |result| {
                        let cookies_vec = match result {
                            Ok(cookies) => cookies
                                .iter()
                                .map(|c| format!("{}={}", c.name, c.value))
                                .collect(),
                            Err(e) => {
                                eprintln!("Error getting cookies: {e}");
                                Vec::new()
                            }
                        };
                        jni_callback(WebViewOutgoingEvent::WebViewCallback(
                            url.clone(),
                            cookies_vec,
                        ));
                    })
                    .unwrap_or_else(|e| {
                        eprintln!("Error getting cookies: {e}");
                    });
            }
        }

        WebViewIncomingEvent::Close | WebViewIncomingEvent::Quit => {
            // Full teardown: end the message loop so `event_loop()` returns
            clear_browsing_data(&state.borrow());
            state.take();
            windows_window::quit();
        }
    }
}

fn clear_browsing_data(state: &Option<SessionState>) {
    if let Some(SessionState { webview, .. }) = state
        && let Ok(profile) = webview.profile()
    {
        let _ = profile.clear_browsing_data_all(|_| {});
    }
}
