#[cfg(target_os = "linux")]
mod gtk4_loop;
#[cfg(target_os = "windows")]
mod tao_loop;
mod webview_event;

#[cfg(target_os = "linux")]
use gtk4_loop as platform_loop;
#[cfg(target_os = "windows")]
use tao_loop as platform_loop;

use jni::EnvUnowned;
use jni::jni_sig;
use jni::jni_str;
use jni::objects::JObjectArray;
use jni::objects::{JClass, JString};
use jni::sys::jint;

use crate::webview_event::{WebViewIncomingEvent, WebViewOutgoingEvent};

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_arn_scrobble_DesktopWebView_startEventLoop(
    mut unowned_env: EnvUnowned,
    _class: JClass,
) {
    // on nvidia and nvidia-open drivers, set WEBKIT_DISABLE_DMABUF_RENDERER=1
    // by checking if /proc/driver/nvidia/version exists

    #[cfg(target_os = "linux")]
    if std::path::Path::new("/proc/driver/nvidia/version").exists() {
        unsafe { std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1") };
        unsafe { std::env::set_var("GSK_RENDERER", "cairo") };
        eprintln!("Using nvidia driver workaround");
    }

    unowned_env
        .with_env(|env| -> jni::errors::Result<()> {
            let jvm = env.get_java_vm()?;
            platform_loop::event_loop(move |event| {
                jvm.attach_current_thread(|env| -> jni::errors::Result<()> {
                    let class = jni_str!("com/arn/scrobble/DesktopWebView");

                    match event {
                        WebViewOutgoingEvent::WebViewCallback(url, cookies_vec) => {
                            let url = JString::from_str(env, url)?;

                            let cookies = JObjectArray::<JString>::new(
                                env,
                                cookies_vec.len(),
                                JString::null(),
                            )?;

                            for (i, cookie) in cookies_vec.into_iter().enumerate() {
                                let cookie_str = JString::from_str(env, cookie)?;
                                cookies.set_element(env, i, cookie_str)?;
                            }

                            env.call_static_method(
                                class,
                                jni_str!("onCallback"),
                                jni_sig!("(Ljava/lang/String;[Ljava/lang/String;)V"),
                                &[(&url).into(), (&cookies).into()],
                            )?;
                        }
                    }
                    Ok(())
                })
                .unwrap();
            });
            Ok(())
        })
        .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_arn_scrobble_DesktopWebView_launchWebView(
    mut unowned_env: EnvUnowned,
    _class: JClass,
    url: JString,
    callback_prefix: JString,
    cookies_url: JString,
    data_dir: JString,
    proxy_type: jint,
    proxy_host: JString,
    proxy_port: jint,
) {
    unowned_env
        .with_env(|env| -> jni::errors::Result<()> {
            let url: String = url.mutf8_chars(env)?.into();
            let callback_prefix: String = callback_prefix.mutf8_chars(env)?.into();
            let cookies_url: String = cookies_url.mutf8_chars(env)?.into();
            let data_dir: String = data_dir.mutf8_chars(env)?.into();
            let proxy_host: String = proxy_host.mutf8_chars(env)?.into();
            let proxy_type_string = match proxy_type {
                0 => "",
                1 => "http",
                2 => "socks5",
                _ => {
                    eprintln!("Invalid proxy type: {proxy_type}");
                    ""
                }
            };

            platform_loop::send_incoming_webview_event(WebViewIncomingEvent::LaunchWebView(
                url,
                callback_prefix,
                cookies_url,
                data_dir,
                proxy_type_string.to_string(),
                proxy_host,
                proxy_port,
            ));
            Ok(())
        })
        .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_arn_scrobble_DesktopWebView_close(
    _env: EnvUnowned,
    _class: JClass,
) {
    platform_loop::send_incoming_webview_event(WebViewIncomingEvent::Close);
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_arn_scrobble_DesktopWebView_quit(_env: EnvUnowned, _class: JClass) {
    platform_loop::send_incoming_webview_event(WebViewIncomingEvent::Quit);
}
