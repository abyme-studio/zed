//! The app's activity, as GPUI reaches it: the process's Java VM, the activity, its assets, its files
//! folder and its configuration, kept by `start`, and the thread GPUI runs on.
//!
//! The app's own `Activity` calls `start` from `onCreate` (through a JNI function of the app's). The
//! first call starts GPUI's thread, which prepares a looper of its own (Choreographer's frame
//! callbacks are posted to it, and `wake` wakes it) and runs the app's `main`. A later call, from an
//! activity Android made again in the same process, keeps the new activity and starts nothing: GPUI
//! goes on, and the new activity's view hands its surface over as the first did.

use std::path::PathBuf;
use std::ptr::NonNull;
use std::sync::{Mutex, OnceLock};

use jni::objects::{GlobalRef, JObject, JString};
use jni::{JNIEnv, JavaVM};
use ndk::asset::AssetManager;
use ndk::configuration::{Configuration, UiModeNight};
use ndk::looper::{ForeignLooper, ThreadLooper};

struct Host {
    vm: JavaVM,
    /// The activity now: a new one replaces it, and the one before is kept alive (never deleted),
    /// as a raw reference to it may still be in use on GPUI's thread.
    activity: Mutex<GlobalRef>,
    /// The activity's `AssetManager`, held so its native one lives as long as the process.
    _assets: GlobalRef,
    assets: NonNull<ndk_sys::AAssetManager>,
    files: PathBuf,
    config: Mutex<Config>,
}

// SAFETY: the AAssetManager is the process's, valid while `_assets` holds its Java object, and
// AAssetManager's calls may be made from any thread.
unsafe impl Send for Host {}
unsafe impl Sync for Host {}

/// What GPUI reads of the activity's configuration.
#[derive(Clone, Copy)]
pub(crate) struct Config {
    /// Device pixels per dp: Android's dp is 1/160 inch.
    pub scale: f32,
    pub dark: bool,
}

static HOST: OnceLock<Host> = OnceLock::new();
static LOOPER: OnceLock<ForeignLooper> = OnceLock::new();

/// Keeps `activity` for GPUI and, the first time, starts GPUI's thread, which runs `main`. Call it
/// from the activity's `onCreate`, before its view hands over a surface.
///
/// `main` runs once per process: it starts GPUI (`gpui_platform::application().run(…)`), which
/// returns only when the activity finishes, and the process ends then (`run`).
pub fn start(env: &mut JNIEnv, activity: &JObject, main: impl FnOnce() + Send + 'static) {
    if let Some(host) = HOST.get() {
        match env.new_global_ref(activity) {
            Ok(new) => std::mem::forget(std::mem::replace(&mut *host.activity.lock().unwrap_or_else(|e| e.into_inner()), new)),
            Err(e) => log::error!("gpui_android: the new activity is not reachable: {e}"),
        }
        return;
    }
    let host = match keep(env, activity) {
        Ok(host) => host,
        Err(e) => {
            log::error!("gpui_android: cannot start, the activity is not reachable: {e}");
            return;
        }
    };
    if HOST.set(host).is_err() {
        return;
    }
    let started = std::thread::Builder::new().name("gpui".into()).spawn(move || {
        let Some(host) = HOST.get() else { return };
        // GPUI's thread calls Java (the clipboard, the keyboard, TalkBack): it stays attached.
        if let Err(e) = host.vm.attach_current_thread_permanently() {
            log::error!("gpui_android: GPUI's thread cannot reach Java: {e}");
            return;
        }
        let looper = ThreadLooper::prepare();
        let _ = LOOPER.set(looper.as_foreign().clone());
        main();
    });
    if let Err(e) = started {
        log::error!("gpui_android: cannot start GPUI's thread: {e}");
    }
}

fn keep(env: &mut JNIEnv, activity: &JObject) -> jni::errors::Result<Host> {
    let vm = env.get_java_vm()?;
    let kept = env.new_global_ref(activity)?;
    let assets = env.call_method(activity, "getAssets", "()Landroid/content/res/AssetManager;", &[])?.l()?;
    // SAFETY: a live AssetManager object on this thread's env; its native manager lives as long as it.
    let native = unsafe { ndk_sys::AAssetManager_fromJava(env.get_raw().cast(), assets.as_raw().cast()) };
    let native = NonNull::new(native).ok_or(jni::errors::Error::NullPtr("the activity's AAssetManager"))?;
    let assets = env.new_global_ref(assets)?;
    let files = env.call_method(activity, "getFilesDir", "()Ljava/io/File;", &[])?.l()?;
    let path = env.call_method(&files, "getAbsolutePath", "()Ljava/lang/String;", &[])?.l()?;
    let files = PathBuf::from(String::from(env.get_string(&JString::from(path))?));
    let config = Mutex::new(read_config(native));
    Ok(Host { vm, activity: Mutex::new(kept), _assets: assets, assets: native, files, config })
}

fn host() -> &'static Host {
    HOST.get().expect("gpui_android: the app's activity calls gpui_android::start before GPUI starts")
}

fn read_config(assets: NonNull<ndk_sys::AAssetManager>) -> Config {
    // SAFETY: the process's asset manager, valid for its life.
    let assets = unsafe { AssetManager::from_ptr(assets) };
    let config = Configuration::from_asset_manager(&assets);
    Config {
        scale: config.density().map_or(1.0, |dpi| dpi as f32 / 160.0),
        dark: config.ui_mode_night() == UiModeNight::Yes,
    }
}

/// The activity's configuration as last read.
pub(crate) fn config() -> Config {
    *host().config.lock().unwrap_or_else(|e| e.into_inner())
}

/// Reads the configuration again (the activity's `onConfigurationChanged`).
pub(crate) fn reread_config() -> Config {
    let host = host();
    let config = read_config(host.assets);
    *host.config.lock().unwrap_or_else(|e| e.into_inner()) = config;
    config
}

/// The process's Java VM.
pub fn vm() -> &'static JavaVM {
    &host().vm
}

/// The activity now, as a raw JNI reference: a global one, never deleted.
pub fn activity() -> jni::sys::jobject {
    host().activity.lock().unwrap_or_else(|e| e.into_inner()).as_obj().as_raw()
}

/// The APK's assets.
pub fn assets() -> AssetManager {
    // SAFETY: the process's asset manager, valid for its life.
    unsafe { AssetManager::from_ptr(host().assets) }
}

/// The activity's files folder (`Context.getFilesDir`).
pub fn files_dir() -> PathBuf {
    host().files.clone()
}

/// Wakes GPUI's thread from any thread: something is queued for it.
pub(crate) fn wake() {
    if let Some(looper) = LOOPER.get() {
        looper.wake();
    }
}

/// Waits on GPUI's thread until it is woken or a callback posted to its looper ran.
pub(crate) fn wait() {
    if let Some(looper) = ThreadLooper::for_thread()
        && let Err(e) = looper.poll_once()
    {
        log::warn!("gpui_android: GPUI's looper: {e}");
    }
}
