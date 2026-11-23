//! GPUI's platform on Android, on the app's own `Activity` and view: the app calls `start` from its
//! activity, and every window, the activity's own (id 0) and each after it, tells GPUI of its
//! surface, input and lifecycle through one queue (`others`).

pub mod a11y;
mod clipboard;
mod dispatcher;
mod display;
mod host;
mod input;
pub mod others;
mod platform;
mod text_input;
mod window;

use std::cell::{Cell, RefCell};
pub(crate) use display::AndroidDisplay;
pub use host::{activity, assets, files_dir, start, vm};
pub use platform::AndroidPlatform;
pub(crate) use window::AndroidWindow;

/// Returns the native platform implementation for Android. On GPUI's thread, after `start`.
pub fn current_platform(_headless: bool) -> std::rc::Rc<dyn gpui::Platform> {
    std::rc::Rc::new(AndroidPlatform::new())
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
