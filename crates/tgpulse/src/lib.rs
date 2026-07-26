//! The front end as a library, for platforms that do not start at `main`.
//!
//! Android launches a shared object and calls `android_main` with a handle to
//! the activity, so the event loop has to be built from that handle rather
//! than created outright. Everything after that point is the same code the
//! desktop binary runs.

#[cfg(target_os = "android")]
mod app;
#[cfg(target_os = "android")]
mod attract;
#[cfg(target_os = "android")]
mod bindings;
#[cfg(target_os = "android")]
mod cli;
#[cfg(target_os = "android")]
mod gui;
#[cfg(target_os = "android")]
mod input;
#[cfg(target_os = "android")]
mod platform;
#[cfg(target_os = "android")]
mod settings;
#[cfg(target_os = "android")]
mod touch;

#[cfg(target_os = "android")]
#[no_mangle]
fn android_main(android_app: winit::platform::android::activity::AndroidApp) {
    use winit::platform::android::EventLoopBuilderExtAndroid as _;

    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Info),
    );

    // The application's private storage is the only writable location, so the
    // ROM directory, NVRAM and save states all live under it.
    let base = android_app
        .external_data_path()
        .or_else(|| android_app.internal_data_path())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    if let Err(e) = std::env::set_current_dir(&base) {
        log::error!(target: "app", "cannot use {}: {e}", base.display());
    }
    let _ = std::fs::create_dir_all(base.join(tgpulse_core::library::DEFAULT_DIR));

    let event_loop = winit::event_loop::EventLoopBuilder::new()
        .with_android_app(android_app)
        .build()
        .expect("event loop");

    let config = tgpulse_core::config::Config {
        // A phone screen is the whole display; there is no window to size.
        fullscreen: true,
        ..Default::default()
    };
    if let Err(e) = app::run_with(event_loop, config, None) {
        log::error!(target: "app", "{e}");
    }
}
