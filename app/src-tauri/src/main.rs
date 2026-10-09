// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // WebKitGTK's DMA-BUF renderer kills the app at startup on Wayland with
    // NVIDIA drivers ("Error 71 dispatching to Wayland display"). Falling back
    // to the older renderer works everywhere; the user can still override it.
    #[cfg(target_os = "linux")]
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        // SAFETY: no other thread exists yet, so nothing can read the
        // environment concurrently.
        unsafe { std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1") };
    }

    sealed_diary_app_lib::run()
}
