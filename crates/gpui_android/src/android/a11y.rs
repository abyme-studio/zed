//! GPUI's accessibility tree, for TalkBack and autofill, one tree per window. Each window has
//! AccessKit's Android adapter here, by its id; the app's view for that window (`AbymeView`) answers
//! Android as the window's `AccessibilityNodeProvider` by calling `node`, `focus`, `act` and `hover`
//! on Android's main thread. GPUI's thread updates the tree each frame it builds one (`update`); the
//! events the change makes are kept, and the app is told (`on_changed`) so the view raises them
//! from its own thread (`raise`). No delegate is injected and no view is looked for.
//!
//! GPUI's node bounds are in device pixels from the window's origin; each view fills its window,
//! edge to edge, so they need no change.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use accesskit::{ActionHandler, ActionRequest, ActivationHandler, TreeUpdate};
use accesskit_android::{Adapter, PlatformAction, QueuedEvents};
use jni::JNIEnv;
use jni::objects::JObject;
use jni::sys::{jboolean, jfloat, jint, JNI_FALSE, JNI_TRUE};

struct Tree {
    adapter: Adapter,
    activation: Activation,
    action: Action,
    /// Events GPUI's updates made, for the view to raise on Android's main thread.
    pending: Vec<QueuedEvents>,
}

/// The most changes' events kept for a view to raise.
const KEPT: usize = 32;

static TREES: Mutex<Option<HashMap<u64, Arc<Mutex<Tree>>>>> = Mutex::new(None);
static CHANGED: OnceLock<Box<dyn Fn(u64) + Send + Sync>> = OnceLock::new();

fn tree(id: u64) -> Option<Arc<Mutex<Tree>>> {
    TREES.lock().unwrap_or_else(|e| e.into_inner()).as_ref()?.get(&id).cloned()
}

/// Called on GPUI's thread when window `id`'s tree made events: the app asks its view for the window
/// to `raise` them on Android's main thread.
pub fn on_changed(changed: impl Fn(u64) + Send + Sync + 'static) {
    let _ = CHANGED.set(Box::new(changed));
}

/// Window `id`'s tree, made when GPUI starts accessibility for it.
pub(crate) fn open(id: u64, callbacks: gpui::A11yCallbacks) {
    let tree = Tree {
        adapter: Adapter::default(),
        activation: Activation(callbacks.activation),
        action: Action(callbacks.action),
        pending: Vec::new(),
    };
    TREES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(id, Arc::new(Mutex::new(tree)));
}

/// Window `id` closed: its tree goes.
pub(crate) fn close(id: u64) {
    if let Some(trees) = TREES.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        trees.remove(&id);
    }
}

/// GPUI's frame's tree for window `id`, on GPUI's thread: applied once Android has asked for the
/// tree, its events kept for `raise`.
pub(crate) fn update(id: u64, update: TreeUpdate) {
    let Some(tree) = tree(id) else { return };
    {
        let mut tree = tree.lock().unwrap_or_else(|e| e.into_inner());
        let Some(events) = tree.adapter.update_if_active(|| update) else { return };
        // A view not yet attached raises none: the oldest go, so they cannot pile up.
        if tree.pending.len() >= KEPT {
            drop(tree.pending.remove(0));
        }
        tree.pending.push(events);
    }
    if let Some(changed) = CHANGED.get() {
        changed(id);
    }
}

/// Raises window `id`'s kept events from its view `host`, on Android's main thread.
pub fn raise(env: &mut JNIEnv, host: &JObject, id: u64) {
    let Some(tree) = tree(id) else { return };
    let pending = std::mem::take(&mut tree.lock().unwrap_or_else(|e| e.into_inner()).pending);
    for events in pending {
        events.raise(env, host);
    }
}

/// `AccessibilityNodeProvider.createAccessibilityNodeInfo` for window `id`'s view `host`; null for
/// no such node, or no tree.
pub fn node<'local>(env: &mut JNIEnv<'local>, host: &JObject, id: u64, virtual_view_id: jint) -> JObject<'local> {
    let Some(tree) = tree(id) else { return JObject::null() };
    let mut tree = tree.lock().unwrap_or_else(|e| e.into_inner());
    let Tree { adapter, activation, .. } = &mut *tree;
    adapter.create_accessibility_node_info(activation, env, host, virtual_view_id)
}

/// `AccessibilityNodeProvider.findFocus` for window `id`'s view `host`.
pub fn focus<'local>(env: &mut JNIEnv<'local>, host: &JObject, id: u64, focus_type: jint) -> JObject<'local> {
    let Some(tree) = tree(id) else { return JObject::null() };
    let mut tree = tree.lock().unwrap_or_else(|e| e.into_inner());
    let Tree { adapter, activation, .. } = &mut *tree;
    adapter.find_focus(activation, env, host, focus_type)
}

/// `AccessibilityNodeProvider.performAction` for window `id`'s view `host`: whether it was done.
pub fn act(env: &mut JNIEnv, host: &JObject, id: u64, virtual_view_id: jint, action: jint, arguments: &JObject) -> jboolean {
    let Some(action) = PlatformAction::from_java(env, action, arguments) else { return JNI_FALSE };
    let Some(tree) = tree(id) else { return JNI_FALSE };
    let events = {
        let mut tree = tree.lock().unwrap_or_else(|e| e.into_inner());
        let Tree { adapter, action: handler, .. } = &mut *tree;
        adapter.perform_action(handler, virtual_view_id, &action)
    };
    let Some(events) = events else { return JNI_FALSE };
    events.raise(env, host);
    JNI_TRUE
}

/// A hover on window `id`'s view `host` (`dispatchHoverEvent`), as TalkBack's explore by touch
/// sends it: whether the tree took it.
pub fn hover(env: &mut JNIEnv, host: &JObject, id: u64, action: jint, x: jfloat, y: jfloat) -> jboolean {
    let Some(tree) = tree(id) else { return JNI_FALSE };
    let events = {
        let mut tree = tree.lock().unwrap_or_else(|e| e.into_inner());
        let Tree { adapter, activation, .. } = &mut *tree;
        adapter.on_hover_event(activation, action, x, y)
    };
    let Some(events) = events else { return JNI_FALSE };
    events.raise(env, host);
    JNI_TRUE
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
