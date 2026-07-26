//! Where the ROM library lives on Android, and the permission that puts it
//! within the player's reach.
//!
//! The application's private directory is walled off: since Android 11 no file
//! manager may browse another app's `Android/data`, so romsets dropped there
//! can only get in over adb. The way out is the All files access grant
//! (`MANAGE_EXTERNAL_STORAGE`), which lets the library live in an ordinary
//! shared folder -- `TGPulse/roms` at the root of shared storage -- that any
//! file manager, and USB file transfer, can read and write.
//!
//! The grant is not a runtime dialog; the app has to send the player to its
//! page in the system settings. That page is opened once, automatically, the
//! first time the app runs without the grant. When the player comes back the
//! grant is picked up on resume and the library moves to the shared folder
//! without a restart. Declining is a valid choice: the private directory keeps
//! working and the app does not ask again.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use jni::objects::{JObject, JString, JValue};
use jni::JNIEnv;
use winit::platform::android::activity::AndroidApp;

/// The grant and `Environment.isExternalStorageManager()` exist from
/// Android 11 (API 30). Older releases keep the private directory.
const ALL_FILES_ACCESS_SDK: i32 = 30;

/// Name of the shared-storage folder the library lives in.
const SHARED_DIR: &str = "TGPulse";

/// Marker left in the private directory once the settings page has been
/// opened, so a player who declined is not sent there on every launch.
const ASKED_MARKER: &str = ".storage-asked";

/// The activity and the VM, captured once at start-up. Both are created before
/// `android_main` runs and outlive the process, so the raw pointers stay valid
/// for as long as anything here can be called.
struct Handle(*mut std::ffi::c_void, *mut std::ffi::c_void);
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}
static HANDLE: OnceLock<Handle> = OnceLock::new();

pub fn init(app: &AndroidApp) {
    let _ = HANDLE.set(Handle(app.vm_as_ptr(), app.activity_as_ptr()));
}

/// Runs `f` against the activity's JNI environment.
fn with_jni<T>(
    f: impl FnOnce(&mut JNIEnv, &JObject) -> jni::errors::Result<T>,
) -> Option<T> {
    let handle = HANDLE.get()?;
    let vm = unsafe { jni::JavaVM::from_raw(handle.0 as *mut jni::sys::JavaVM) }.ok()?;
    let mut env = vm.attach_current_thread().ok()?;
    // The activity reference is global and belongs to the runtime; wrapping it
    // and then forgetting the wrapper keeps the drop from deleting it.
    let activity = unsafe { JObject::from_raw(handle.1 as jni::sys::jobject) };
    let out = f(&mut env, &activity);
    std::mem::forget(activity);
    match out {
        Ok(value) => Some(value),
        Err(e) => {
            log::error!(target: "app", "storage bridge failed: {e}");
            None
        }
    }
}

fn sdk_int() -> i32 {
    with_jni(|env, _| {
        Ok(env
            .get_static_field("android/os/Build$VERSION", "SDK_INT", "I")?
            .i()?)
    })
    .unwrap_or(0)
}

/// Whether the app holds the All files access grant.
pub fn has_all_files_access() -> bool {
    sdk_int() >= ALL_FILES_ACCESS_SDK
        && with_jni(|env, _| {
            Ok(env
                .call_static_method(
                    "android/os/Environment",
                    "isExternalStorageManager",
                    "()Z",
                    &[],
                )?
                .z()?)
        })
        .unwrap_or(false)
}

/// The root of shared storage (`/sdcard` on virtually every device), asked of
/// the system rather than assumed.
fn shared_root() -> Option<PathBuf> {
    with_jni(|env, _| {
        let file = env
            .call_static_method(
                "android/os/Environment",
                "getExternalStorageDirectory",
                "()Ljava/io/File;",
                &[],
            )?
            .l()?;
        let path = env
            .call_method(&file, "getAbsolutePath", "()Ljava/lang/String;", &[])?
            .l()?;
        Ok(PathBuf::from(String::from(
            env.get_string(&JString::from(path))?,
        )))
    })
}

/// The library's home once the grant is held: `<shared>/TGPulse/roms`.
pub fn shared_rom_dir() -> Option<PathBuf> {
    shared_root().map(|root| root.join(SHARED_DIR).join(tgpulse_core::library::DEFAULT_DIR))
}

/// Turns the display between the two landscapes.
///
/// The manifest asks for the ordinary one so the activity starts right side
/// up on a bare phone; this overrides it at runtime. `8` and `0` are the
/// platform's `SCREEN_ORIENTATION_REVERSE_LANDSCAPE` and
/// `SCREEN_ORIENTATION_LANDSCAPE`.
pub fn set_reverse_landscape(reverse: bool) {
    let orientation = if reverse { 8 } else { 0 };
    with_jni(|env, activity| {
        env.call_method(
            activity,
            "setRequestedOrientation",
            "(I)V",
            &[JValue::Int(orientation)],
        )?;
        Ok(())
    });
}

/// Opens the app's All files access page in the system settings, the first
/// time only. `private_dir` is where the "already asked" marker lives.
pub fn request_all_files_access_once(private_dir: &Path) {
    if sdk_int() < ALL_FILES_ACCESS_SDK {
        return;
    }
    let marker = private_dir.join(ASKED_MARKER);
    if marker.exists() {
        return;
    }
    let _ = std::fs::write(&marker, b"");
    request_all_files_access();
}

fn request_all_files_access() {
    let opened = with_jni(|env, activity| {
        let action = env.new_string("android.settings.MANAGE_APP_ALL_FILES_ACCESS_PERMISSION")?;
        // The package id from Cargo.toml's `package.metadata.android`.
        let package = env.new_string("package:org.tgpulse.emulator")?;
        let uri = env
            .call_static_method(
                "android/net/Uri",
                "parse",
                "(Ljava/lang/String;)Landroid/net/Uri;",
                &[JValue::Object(&package)],
            )?
            .l()?;
        let intent = env.new_object(
            "android/content/Intent",
            "(Ljava/lang/String;Landroid/net/Uri;)V",
            &[JValue::Object(&action), JValue::Object(&uri)],
        )?;
        env.call_method(
            activity,
            "startActivity",
            "(Landroid/content/Intent;)V",
            &[JValue::Object(&intent)],
        )?;
        if env.exception_check()? {
            // A few builds ship no handler for the app-specific page; the
            // generic All files access list is the fallback.
            env.exception_clear()?;
            let action = env.new_string("android.settings.MANAGE_ALL_FILES_ACCESS_PERMISSION")?;
            let intent = env.new_object(
                "android/content/Intent",
                "(Ljava/lang/String;)V",
                &[JValue::Object(&action)],
            )?;
            env.call_method(
                activity,
                "startActivity",
                "(Landroid/content/Intent;)V",
                &[JValue::Object(&intent)],
            )?;
        }
        Ok(())
    });
    if opened.is_none() {
        log::error!(target: "app", "could not open the All files access settings");
    }
}
