#[cfg(target_os = "linux")]
use ashpd::desktop::file_chooser::{FileFilter, OpenFileRequest};
#[cfg(target_os = "windows")]
use windows::core::{HSTRING, Interface};
#[cfg(target_os = "windows")]
use windows::{
    Foundation::Uri,
    Storage::Pickers::{FileOpenPicker, FileSavePicker},
    Win32::{Foundation::HWND, UI::Shell::IInitializeWithWindow},
};

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
