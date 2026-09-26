use std::cell::RefCell;
use std::rc::Rc;
use std::sync::OnceLock;

use gtk4::prelude::*;
use webkit6::prelude::*;
use webkit6::{
    NavigationPolicyDecision, NetworkProxyMode, NetworkProxySettings, NetworkSession,
    PolicyDecisionType, WebView,
};

use crate::webview_event::{WebViewIncomingEvent, WebViewOutgoingEvent};

static INCOMING_EVENT_SENDER: OnceLock<async_channel::Sender<WebViewIncomingEvent>> =
    OnceLock::new();
static WEBVIEW_INITIAL_EVENT: OnceLock<WebViewIncomingEvent> = OnceLock::new();

pub fn send_incoming_webview_event(event: WebViewIncomingEvent) {
    if let Some(sender) = INCOMING_EVENT_SENDER.get() {
        if let Err(e) = sender.try_send(event) {
            eprintln!("Error sending event to gtk4 loop: {e}");
        }
    } else {
        let _ = WEBVIEW_INITIAL_EVENT.set(event);
        eprintln!("GTK4 event loop not running yet");
    }
}

pub fn event_loop(jni_callback: impl Fn(WebViewOutgoingEvent) + 'static) {
    gtk4::init().expect("Failed to initialize GTK4");

    let (sender, receiver) = async_channel::bounded(32);
    INCOMING_EVENT_SENDER
        .set(sender)
        .expect("event_loop called more than once");

    // Replay any event that arrived before the loop started
    if let Some(event) = WEBVIEW_INITIAL_EVENT.get() {
        send_incoming_webview_event(event.clone());
    }

    let context = webkit6::glib::MainContext::default();
    let main_loop = webkit6::glib::MainLoop::new(Some(&context), false);
    let jni_callback = Rc::new(jni_callback);
    let main_loop_quit = main_loop.clone();

    // Keeps the window, webview, and network session alive for the session
    let state: Rc<RefCell<Option<(gtk4::Window, WebView, NetworkSession)>>> =
        Rc::new(RefCell::new(None));

    // with_thread_default acquires the context on this thread so that
    // spawn_local is permitted, then runs the main loop inside.
    context
        .with_thread_default(|| {
            context.spawn_local(async move {
                while let Ok(event) = receiver.recv().await {
                    match event {
                        WebViewIncomingEvent::LaunchWebView(
                            url,
                            callback_prefix,
                            cookies_url,
                            _data_dir,
                            proxy_type,
                            proxy_host,
                            proxy_port,
                        ) => {
                            let session = NetworkSession::new_ephemeral();

                            if !proxy_type.is_empty() && !proxy_host.is_empty() && proxy_port != 0 {
                                let proxy_uri = format!("{proxy_type}://{proxy_host}:{proxy_port}");
                                let proxy_settings =
                                    NetworkProxySettings::new(Some(&proxy_uri), &[] as &[&str]);
                                session.set_proxy_settings(
                                    NetworkProxyMode::Custom,
                                    Some(&proxy_settings),
                                );
                            }

                            let cookie_manager = session
                                .cookie_manager()
                                .expect("NetworkSession has no CookieManager");

                            let webview = WebView::builder().network_session(&session).build();
                            webview.load_uri(&url);

                            let jni_cb = jni_callback.clone();
                            let cookie_manager_cb = cookie_manager.clone();
                            let callback_prefix_nav = callback_prefix.clone();
                            let cookies_url_nav = cookies_url.clone();

                            webview.connect_decide_policy(
                                move |_webview, decision, decision_type| {
                                    if decision_type != PolicyDecisionType::NavigationAction {
                                        return false;
                                    }
                                    let Some(nav) =
                                        decision.downcast_ref::<NavigationPolicyDecision>()
                                    else {
                                        return false;
                                    };
                                    let Some(action) = nav.navigation_action() else {
                                        return false;
                                    };
                                    let Some(request) = action.request() else {
                                        return false;
                                    };
                                    let Some(uri_gs) = request.uri() else {
                                        return false;
                                    };
                                    let uri = uri_gs.to_string();

                                    if !uri.starts_with(&callback_prefix_nav) {
                                        return false;
                                    }

                                    nav.ignore();

                                    let cookie_mgr = cookie_manager_cb.clone();
                                    let cookies_url = cookies_url_nav.clone();
                                    let jni_cb = jni_cb.clone();

                                    webkit6::glib::MainContext::default().spawn_local(async move {
                                        let cookies: Vec<String> = cookie_mgr
                                            .cookies_future(&cookies_url)
                                            .await
                                            .unwrap_or_default()
                                            .into_iter()
                                            .map(|mut c| {
                                                format!(
                                                    "{}={}",
                                                    c.name().unwrap_or_default(),
                                                    c.value().unwrap_or_default()
                                                )
                                            })
                                            .collect();
                                        jni_cb(WebViewOutgoingEvent::WebViewCallback(uri, cookies));
                                    });

                                    true
                                },
                            );

                            let dpi = gtk4::Settings::default()
                                .map(|s| s.gtk_xft_dpi() as f64 / 1024.0)
                                .filter(|d| *d > 0.0)
                                .unwrap_or(96.0);

                            let scale = dpi / 96.0;

                            let window = gtk4::Window::builder()
                                .title("WebView")
                                .default_width((800.0 * scale) as i32)
                                .default_height((600.0 * scale) as i32)
                                .child(&webview)
                                .build();

                            window.present();
                            *state.borrow_mut() = Some((window, webview, session));
                        }

                        WebViewIncomingEvent::Close => {
                            if let Some((window, _webview, _session)) = state.take() {
                                window.close();
                            }
                        }

                        WebViewIncomingEvent::Quit => {
                            if let Some((window, _webview, _session)) = state.take() {
                                window.close();
                            }
                            main_loop_quit.quit();
                        }
                    }
                }
            });

            main_loop.run();
        })
        .expect("Failed to set GTK4 main context as thread default");
}
