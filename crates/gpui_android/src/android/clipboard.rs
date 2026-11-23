//! The system clipboard, over JNI: `ClipboardManager`'s primary clip, as plain text. Android keeps no
//! metadata beside it, so the last item GPUI wrote is kept here and given back whole while the
//! system's text is still its text.

use gpui::ClipboardItem;
use jni::objects::{JObject, JString, JValue};

/// The system clipboard's text; `None` when it holds none, or the clipboard cannot be read.
pub(crate) fn read() -> Option<String> {
    with_env(|env, activity| {
        let manager = manager(env, activity)?;
        let clip = env.call_method(&manager, "getPrimaryClip", "()Landroid/content/ClipData;", &[])?.l()?;
        if clip.is_null() || env.call_method(&clip, "getItemCount", "()I", &[])?.i()? == 0 {
            return Ok(None);
        }
        let item = env
            .call_method(&clip, "getItemAt", "(I)Landroid/content/ClipData$Item;", &[JValue::Int(0)])?
            .l()?;
        // coerceToText turns a URI or an intent into text as well.
        let text = env
            .call_method(
                &item,
                "coerceToText",
                "(Landroid/content/Context;)Ljava/lang/CharSequence;",
                &[JValue::Object(activity)],
            )?
            .l()?;
        if text.is_null() {
            return Ok(None);
        }
        let text = env.call_method(&text, "toString", "()Ljava/lang/String;", &[])?.l()?;
        Ok(Some(env.get_string(&JString::from(text))?.into()))
    })
    .flatten()
}

/// Puts `text` on the system clipboard as a plain-text clip.
pub(crate) fn write(text: &str) {
    with_env(|env, activity| {
        let manager = manager(env, activity)?;
        let label = env.new_string("")?;
        let text = env.new_string(text)?;
        let clip = env
            .call_static_method(
                "android/content/ClipData",
                "newPlainText",
                "(Ljava/lang/CharSequence;Ljava/lang/CharSequence;)Landroid/content/ClipData;",
                &[JValue::Object(&label), JValue::Object(&text)],
            )?
            .l()?;
        env.call_method(&manager, "setPrimaryClip", "(Landroid/content/ClipData;)V", &[JValue::Object(&clip)])?;
        Ok(())
    });
}

/// What GPUI reads: the item it last wrote while the system's text is still its text, else the
/// system's text alone.
pub(crate) fn item(last: Option<&ClipboardItem>) -> Option<ClipboardItem> {
    let text = read()?;
    match last {
        Some(item) if item.text().as_deref() == Some(text.as_str()) => Some(item.clone()),
        _ => Some(ClipboardItem::new_string(text)),
    }
}

fn manager<'local>(
    env: &mut jni::JNIEnv<'local>,
    activity: &JObject,
) -> jni::errors::Result<JObject<'local>> {
    let name = env.new_string("clipboard")?;
    env.call_method(
        activity,
        "getSystemService",
        "(Ljava/lang/String;)Ljava/lang/Object;",
        &[JValue::Object(&name)],
    )?
    .l()
}

/// Runs `f` with this thread's JNI env and the activity, in a local frame; a JNI error is logged
/// (and a pending Java exception cleared) and answers `None`.
fn with_env<T>(
    f: impl FnOnce(&mut jni::JNIEnv, &JObject) -> jni::errors::Result<T>,
) -> Option<T> {
    let run = || -> jni::errors::Result<T> {
        let mut env = super::host::vm().attach_current_thread()?;
        let result = env.with_local_frame(16, |env| {
            // SAFETY: a global reference host.rs keeps, never deleted.
            let activity = unsafe { JObject::from_raw(super::host::activity()) };
            f(env, &activity)
        });
        if env.exception_check().unwrap_or(false) {
            let _ = env.exception_clear();
        }
        result
    };
    match run() {
        Ok(value) => Some(value),
        Err(e) => {
            log::warn!("gpui_android: the clipboard failed: {e}");
            None
        }
    }
}
