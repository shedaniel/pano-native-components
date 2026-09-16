cargo build --release
copy target\release\pano_native_components.dll ..\pano-scrobbler\desktopApp\resources\windows-x64\
copy target\release\native_webview.dll ..\pano-scrobbler\desktopApp\resources\windows-x64\
copy target\release\WebView2Loader.dll ..\pano-scrobbler\desktopApp\resources\windows-x64\
