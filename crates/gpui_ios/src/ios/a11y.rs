//! Accessibility: GPUI's AccessKit tree, read by VoiceOver and by XCUITest through AccessKit's iOS
//! adapter, which subclasses the window's view as `gpui_macos` has AccessKit's macOS adapter
//! subclass its window's content view.

use std::cell::RefCell;
use std::ffi::c_void;

use gpui::A11yCallbacks;

/// The adapter on one window's view, once GPUI asks for it.
#[derive(Default)]
pub(super) struct A11y {
    adapter: RefCell<Option<accesskit_ios::SubclassingAdapter>>,
}

impl A11y {
    /// Subclasses `view`, the window's backing `UIView`, for AccessKit. Done before the view
    /// is first focused, as the adapter asks; a second call keeps the first adapter.
    ///
    /// # Safety
    ///
    /// `view` is a live `UIView` that outlives this window.
    pub(super) unsafe fn init(&self, view: *mut c_void, callbacks: A11yCallbacks) {
        if self.adapter.borrow().is_some() {
            return;
        }
        let adapter = unsafe {
            accesskit_ios::SubclassingAdapter::new(
                view,
                Activation(callbacks.activation),
                Action(callbacks.action),
                Deactivation(callbacks.deactivation),
            )
        };
        *self.adapter.borrow_mut() = Some(adapter);
    }

    pub(super) fn update(&self, tree_update: accesskit::TreeUpdate) {
        let events = self
            .adapter
            .borrow()
            .as_ref()
            .and_then(|adapter| adapter.update_if_active(|| tree_update));
        if let Some(events) = events {
            events.raise();
        }
    }
}

struct Activation(Box<dyn Fn() -> Option<accesskit::TreeUpdate> + Send + 'static>);

impl accesskit::ActivationHandler for Activation {
    fn request_initial_tree(&mut self) -> Option<accesskit::TreeUpdate> {
        (self.0)()
    }
}

struct Action(Box<dyn Fn(accesskit::ActionRequest) + Send + 'static>);

impl accesskit::ActionHandler for Action {
    fn do_action(&mut self, request: accesskit::ActionRequest) {
        (self.0)(request);
    }
}

struct Deactivation(Box<dyn Fn() + Send + 'static>);

impl accesskit::DeactivationHandler for Deactivation {
    fn deactivate_accessibility(&mut self) {
        (self.0)();
    }
}
