//! The window's insets, read over JNI from the decor view's root `WindowInsets`: system bars and
//! the display cutout as GPUI's safe area, the keyboard as its IME inset. GameActivity draws edge to
//! edge, so both overlap the surface.

use android_activity::AndroidApp;
use gpui::{Edges, WindowInsets, px};
use jni::JavaVM;
use jni::objects::{JObject, JValue};

/// `WindowInsets.Type.systemBars() | displayCutout()` and `ime()`, API 30 on; their values are
/// fixed in the platform.
const SYSTEM_BARS: i32 = 0x7; // statusBars | navigationBars | captionBar
const DISPLAY_CUTOUT: i32 = 0x80;
const IME: i32 = 0x8;

/// The insets now, in GPUI's pixels at `scale`; `None` when the view has none yet.
pub(crate) fn read(app: &AndroidApp, scale: f32) -> Option<WindowInsets> {
    match read_jni(app, scale) {
        Ok(insets) => insets,
        Err(e) => {
            log::warn!("gpui_android: could not read the window's insets: {e}");
            None
        }
    }
}

fn read_jni(app: &AndroidApp, scale: f32) -> jni::errors::Result<Option<WindowInsets>> {
    // SAFETY: the VM and activity pointers are android-activity's, valid while the activity is.
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) }?;
    let mut env = vm.attach_current_thread()?;
    // A local frame, so the references made here are let go of when it returns.
    env.with_local_frame(32, |env| read_in(env, app, scale))
}

fn read_in(
    mut env: &mut jni::JNIEnv,
    app: &AndroidApp,
    scale: f32,
) -> jni::errors::Result<Option<WindowInsets>> {
    // SAFETY: a global reference android-activity holds for the activity's life; not deleted here.
    let activity = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
    let window = env.call_method(&activity, "getWindow", "()Landroid/view/Window;", &[])?.l()?;
    let decor = env.call_method(&window, "getDecorView", "()Landroid/view/View;", &[])?.l()?;
    let insets = env
        .call_method(&decor, "getRootWindowInsets", "()Landroid/view/WindowInsets;", &[])?
        .l()?;
    if insets.is_null() {
        return Ok(None);
    }
    let sdk = env.get_static_field("android/os/Build$VERSION", "SDK_INT", "I")?.i()?;
    let edges = |values: [i32; 4]| Edges {
        left: px(values[0] as f32 / scale),
        top: px(values[1] as f32 / scale),
        right: px(values[2] as f32 / scale),
        bottom: px(values[3] as f32 / scale),
    };
    let (safe_area, ime) = if sdk >= 30 {
        let of_type = |env: &mut jni::JNIEnv, mask: i32| -> jni::errors::Result<[i32; 4]> {
            let value = env
                .call_method(&insets, "getInsets", "(I)Landroid/graphics/Insets;", &[JValue::Int(mask)])?
                .l()?;
            let mut out = [0; 4];
            for (i, field) in ["left", "top", "right", "bottom"].iter().enumerate() {
                out[i] = env.get_field(&value, *field, "I")?.i()?;
            }
            Ok(out)
        };
        let safe = of_type(&mut env, SYSTEM_BARS | DISPLAY_CUTOUT)?;
        let ime = of_type(&mut env, IME)?;
        (edges(safe), edges(ime))
    } else {
        // Before API 30 the system window insets hold the bars and the keyboard together.
        let mut out = [0; 4];
        for (i, getter) in ["getSystemWindowInsetLeft", "getSystemWindowInsetTop", "getSystemWindowInsetRight", "getSystemWindowInsetBottom"]
            .iter()
            .enumerate()
        {
            out[i] = env.call_method(&insets, *getter, "()I", &[])?.i()?;
        }
        (edges(out), Edges::default())
    };
    Ok(Some(WindowInsets { safe_area, ime, ..Default::default() }))
}
