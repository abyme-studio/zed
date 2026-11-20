//! The `GameActivity`-backed implementation of GPUI's platform on Android.

mod dispatcher;
mod display;
mod input;
mod platform;
mod text_input;
mod window;

use std::cell::{Cell, RefCell};
use std::sync::OnceLock;

use android_activity::AndroidApp;

pub(crate) use display::AndroidDisplay;
pub use platform::AndroidPlatform;
pub(crate) use window::AndroidWindow;

static APP: OnceLock<AndroidApp> = OnceLock::new();

/// Hands GPUI the activity `android_main` was given. Call it first thing in `android_main`, before
/// `gpui_platform::application()`.
///
/// A process holds one GPUI app, bound to the first activity's thread: GameActivity polls each
/// activity on a thread of its own, and GPUI cannot move. So the app's manifest lists every
/// `configChanges` (the activity is never made again for a change of configuration), `run` ends the
/// process when its activity is destroyed, and a second activity in the same process ends it too,
/// so the system starts the activity again in a fresh one.
pub fn set_android_app(app: AndroidApp) {
    if APP.set(app).is_err() {
        log::error!("gpui_android: a second activity in one process; ending it for a fresh start");
        std::process::exit(0);
    }
}

pub(crate) fn android_app() -> AndroidApp {
    APP.get()
        .cloned()
        .expect("gpui_android: call set_android_app(app) in android_main before starting GPUI")
}

/// Returns the native platform implementation for Android.
pub fn current_platform(_headless: bool) -> std::rc::Rc<dyn gpui::Platform> {
    std::rc::Rc::new(AndroidPlatform::new(android_app()))
}

/// A callback that may be replaced or withdrawn while it runs, as gpui_ios keeps them.
pub(crate) struct CallbackSlot<T> {
    value: RefCell<Option<T>>,
    generation: Cell<u64>,
}

impl<T> Default for CallbackSlot<T> {
    fn default() -> Self {
        Self { value: RefCell::new(None), generation: Cell::new(0) }
    }
}

impl<T> CallbackSlot<T> {
    pub(crate) fn set(&self, value: T) {
        self.generation.set(self.generation.get().wrapping_add(1));
        drop(self.value.replace(Some(value)));
    }

    pub(crate) fn take(&self) -> Option<T> {
        self.generation.set(self.generation.get().wrapping_add(1));
        self.value.borrow_mut().take()
    }

    pub(crate) fn is_set(&self) -> bool {
        self.value.borrow().is_some()
    }

    pub(crate) fn with<R>(&self, callback: impl FnOnce(&mut T) -> R) -> Option<R> {
        let mut value = self.value.borrow_mut().take()?;
        let generation = self.generation.get();
        let result = callback(&mut value);
        // A handler replaced or removed while it ran stays replaced or removed.
        if self.generation.get() == generation {
            drop(self.value.replace(Some(value)));
        }
        Some(result)
    }
}
