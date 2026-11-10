#[cfg(target_os = "linux")]
use ashpd::desktop::file_chooser::{FileFilter, OpenFileRequest};
#[cfg(target_os = "windows")]
use windows::{
    Foundation::Uri,
    Storage::Pickers::{FileOpenPicker, FileSavePicker},
    Win32::{Foundation::HWND, UI::Shell::IInitializeWithWindow},
};
#[cfg(target_os = "windows")]
use windows_core::{HSTRING, Interface};

#[cfg(target_os = "linux")]
pub async fn launch_file_picker(
    save: bool,
    title: String,
    file_name: String,
    filters: Vec<String>,
) -> String {
    let mut file_filter = FileFilter::new(filters.join(", ").as_str());

    for filter in filters {
        file_filter = file_filter.glob(&filter);
    }

    if !save {
        let request = OpenFileRequest::default()
            .directory(false)
            // .identifier(identifier)
            .modal(true)
            .title(&*title)
            .multiple(false)
            .filters(vec![file_filter]);
        // .current_filter(current_filter)

        match request.send().await.and_then(|r| r.response()) {
            Ok(files) => files
                .uris()
                .first()
                .map(|u| u.to_string())
                .unwrap_or_default(),
            Err(err) => {
                log::error!("Failed to open a file: {err}");
                "".to_string()
            }
        }
    } else {
        use ashpd::desktop::file_chooser::SaveFileRequest;

        let request = SaveFileRequest::default()
            // .identifier(identifier)
            .modal(true)
            .title(&*title)
            .current_name(&*file_name)
            .filters(vec![file_filter]);
        // .current_filter(current_filter)

        match request.send().await.and_then(|r| r.response()) {
            Ok(files) => files
                .uris()
                .first()
                .map(|u| u.to_string())
                .unwrap_or_default(),
            Err(err) => {
                log::error!("Failed to open a file: {err}");
                "".to_string()
            }
        }
    }
}

#[cfg(target_os = "windows")]
pub async fn launch_file_picker(
    hwnd: i64,
    save: bool,
    file_name: String,
    extensions: Vec<String>,
) -> windows::core::Result<HSTRING> {
    let mut extensions_sorted = extensions.clone();
    extensions_sorted.sort();
    let identifier = extensions_sorted.join(",");

    let file = if save {
        let dummy_picker = FileOpenPicker::new()?;
        let filter_vec = dummy_picker.FileTypeFilter()?;

        let picker = FileSavePicker::new()?;
        unsafe {
            picker
                .cast::<IInitializeWithWindow>()?
                .Initialize(HWND(hwnd as _))?;
        }

        for extension in extensions {
            let filter_vec = filter_vec.clone();
            let ext = &HSTRING::from(extension);
            filter_vec.Append(ext)?;
            picker.FileTypeChoices()?.Insert(ext, &filter_vec)?;
        }
        picker.SetSettingsIdentifier(&HSTRING::from(identifier))?;
        picker.SetSuggestedFileName(&HSTRING::from(file_name))?;
        picker.PickSaveFileAsync()?.await?
    } else {
        let picker = FileOpenPicker::new()?;
        unsafe {
            picker
                .cast::<IInitializeWithWindow>()?
                .Initialize(HWND(hwnd as _))?;
        }

        for extension in extensions {
            picker.FileTypeFilter()?.Append(&HSTRING::from(extension))?;
        }
        picker.SetSettingsIdentifier(&HSTRING::from(identifier))?;
        picker.PickSingleFileAsync()?.await?
    };

    let path = file.Path()?;

    Uri::CreateUri(&path)?.AbsoluteUri()
}

// ── macOS ────────────────────────────────────────────────────────────────────
//
// Driven via `osascript` (AppleScript `choose file` / `choose file name`), which
// presents the real NSOpenPanel / NSSavePanel on the main run loop of the
// helper process. This mirrors how the rest of the macOS port shells out to
// system binaries (`osascript` for notifications, `open` for URLs, `ioreg` for
// the machine id) and needs no extra crates or main-thread dispatch from the
// JVM app.
//
// The blocking `osascript` call is run through `tokio::task::spawn_blocking`
// because the macOS listener runs on a current-thread tokio runtime shared with
// the IPC/theme/now-playing tasks — a synchronous `.output()` would stall them
// for the entire duration the panel is open.
//
// Known v1 limitations (follow-ups, not blockers):
//   - No extension filtering on Open (AppleScript `of type` takes UTIs, not
//     file suffixes, and several of our types — .jsonl, .log — have no stable
//     UTI). The Kotlin layer validates the picked file. A future objc2
//     NSOpenPanel impl could use `allowedContentTypes` with `UTType`.
//   - The panel is owned by the `osascript` subprocess, so its title bar is not
//     app-modal w.r.t. the Compose window. Acceptable for v1.

#[cfg(target_os = "macos")]
pub async fn launch_file_picker(
    save: bool,
    title: String,
    file_name: String,
    _filters: Vec<String>,
) -> String {
    let script = build_applescript(save, &title, &file_name);

    // Run the GUI panel off the async runtime; it blocks until the user is done.
    let spawned = tokio::task::spawn_blocking(move || {
        std::process::Command::new("/usr/bin/osascript")
            .arg("-e")
            .arg(&script)
            .output()
    })
    .await;

    let output = match spawned {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => {
            log::error!("Failed to spawn osascript file picker: {e}");
            return String::new();
        }
        Err(e) => {
            log::error!("osascript file picker task failed: {e}");
            return String::new();
        }
    };

    // Cancel (AppleScript error -128) or any other failure → no selection.
    if !output.status.success() {
        return String::new();
    }

    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        return String::new();
    }

    posix_path_to_file_uri(&path)
}

#[cfg(target_os = "macos")]
fn escape_applescript_literal(s: &str) -> String {
    // AppleScript string literals escape backslash first, then the quote.
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(target_os = "macos")]
fn build_applescript(save: bool, title: &str, file_name: &str) -> String {
    let prompt = if title.is_empty() {
        String::new()
    } else {
        format!(" with prompt \"{}\"", escape_applescript_literal(title))
    };

    if save {
        let default_name = if file_name.is_empty() {
            String::new()
        } else {
            format!(
                " default name \"{}\"",
                escape_applescript_literal(file_name)
            )
        };
        // `choose file name` shows a Save-style panel; the file is *not* created —
        // the caller (PlatformFile.overwrite) writes the content afterwards.
        format!("POSIX path of (choose file name{prompt}{default_name})")
    } else {
        format!("POSIX path of (choose file{prompt} without multiple selections allowed)")
    }
}

/// Percent-encode an absolute POSIX path into a `file://` URI that
/// `java.net.URI` (used by `PlatformFile.desktop.kt`) will accept and decode.
#[cfg(target_os = "macos")]
pub(crate) fn posix_path_to_file_uri(path: &str) -> String {
    let mut out = String::with_capacity(path.len() + 7);
    out.push_str("file://");
    for &b in path.as_bytes() {
        let c = b as char;
        if c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.' | '~') {
            out.push(c);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

#[cfg(target_os = "macos")]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_uri_encodes_spaces_and_keeps_slashes() {
        assert_eq!(
            posix_path_to_file_uri("/Users/me/My Collage.png"),
            "file:///Users/me/My%20Collage.png"
        );
    }

    #[test]
    fn applescript_literal_escapes_quotes_and_backslashes() {
        assert_eq!(
            escape_applescript_literal(r#"a"b\c"#),
            r#"a\"b\\c"#
        );
    }

    #[test]
    fn open_script_omits_prompt_when_title_blank() {
        assert_eq!(
            build_applescript(false, "", ""),
            "POSIX path of (choose file without multiple selections allowed)"
        );
    }

    #[test]
    fn save_script_includes_default_name() {
        assert_eq!(
            build_applescript(true, "Create", "Collage.png"),
            "POSIX path of (choose file name with prompt \"Create\" default name \"Collage.png\")"
        );
    }
}
