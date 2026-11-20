//! GPUI's accessibility tree, given to TalkBack through AccessKit's Android adapter. The adapter
//! injects its own `AccessibilityDelegate` (a class from AccessKit's embedded dex, so the app needs no
//! Java of its own) into GameActivity's `SurfaceView`, the view GPUI draws in. GPUI's node bounds
//! are in device pixels from the window's origin; the view fills the window, edge to edge, so they
//! need no change.

use accesskit::{ActionHandler, ActionRequest, ActivationHandler, TreeUpdate};
use accesskit_android::InjectingAdapter;
use android_activity::AndroidApp;
use jni::JavaVM;
use jni::objects::{JObject, JValue};

pub(crate) struct A11y(InjectingAdapter);

impl A11y {
    /// Injects the adapter into the activity's view; `None`, logged, when the view cannot be found.
    pub(crate) fn new(app: &AndroidApp, callbacks: gpui::A11yCallbacks) -> Option<Self> {
        match make(app, callbacks) {
            Ok(adapter) => Some(Self(adapter)),
            Err(e) => {
                log::warn!("gpui_android: no screen reader support, the activity's view was not found: {e}");
                None
            }
        }
    }

    pub(crate) fn update(&mut self, tree_update: TreeUpdate) {
        self.0.update_if_active(|| tree_update);
    }
}

fn make(app: &AndroidApp, callbacks: gpui::A11yCallbacks) -> jni::errors::Result<InjectingAdapter> {
    // SAFETY: the VM and activity pointers are android-activity's, valid while the activity is.
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) }?;
    // The adapter reaches the VM from this thread on every update, so it stays attached.
    let mut env = vm.attach_current_thread_permanently()?;
    env.with_local_frame(16, |env| {
        // SAFETY: a global reference android-activity holds for the activity's life; not deleted here.
        let activity = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
        let view = surface_view(env, &activity)?;
        let activation = Activation(callbacks.activation);
        let action = Action(callbacks.action);
        Ok(InjectingAdapter::new(env, &view, activation, action))
    })
}

/// GameActivity's `SurfaceView`: its content view, the only child of `android.R.id.content`.
fn surface_view<'local>(
    env: &mut jni::JNIEnv<'local>,
    activity: &JObject,
) -> jni::errors::Result<JObject<'local>> {
    let content_id = env.get_static_field("android/R$id", "content", "I")?.i()?;
    let content = env
        .call_method(activity, "findViewById", "(I)Landroid/view/View;", &[JValue::Int(content_id)])?
        .l()?;
    if content.is_null() {
        return Err(jni::errors::Error::NullPtr("android.R.id.content"));
    }
    let view = env
        .call_method(&content, "getChildAt", "(I)Landroid/view/View;", &[JValue::Int(0)])?
        .l()?;
    if view.is_null() {
        return Err(jni::errors::Error::NullPtr("the content view's first child"));
    }
    Ok(view)
}

struct Activation(Box<dyn Fn() -> Option<TreeUpdate> + Send + 'static>);

impl ActivationHandler for Activation {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        (self.0)()
    }
}

struct Action(Box<dyn Fn(ActionRequest) + Send + 'static>);

impl ActionHandler for Action {
    fn do_action(&mut self, request: ActionRequest) {
        (self.0)(request);
    }
}
