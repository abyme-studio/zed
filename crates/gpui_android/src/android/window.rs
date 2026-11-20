//! The activity's `SurfaceView` as GPUI's one window, drawn through `gpui_wgpu`.
//!
//! The surface comes and goes with the activity: `GameActivity` hands over an `ANativeWindow` on
//! `InitWindow` and takes it back on `TerminateWindow` (on pause, on rotation). The renderer is made
//! with the first surface and outlives the rest: a lost surface only unconfigures it, keeping its
//! device and atlas, and the next one is configured in its place.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use android_activity::AndroidApp;
use gpui::{
    AnyWindowHandle, Bounds, Capslock, DevicePixels, DispatchEventResult, GpuSpecs, HeadlessAtlas,
    Modifiers, Pixels, PlatformAtlas, PlatformDisplay, PlatformInput, PlatformInputHandler,
    PlatformWindow, Point, PromptButton, PromptLevel, RequestFrameOptions, Scene, Size,
    WindowAppearance, WindowBackgroundAppearance, WindowBounds, WindowControlArea, WindowInsets,
    WindowParams, WindowVisibility, px, size,
};
use gpui_wgpu::{GpuContext, WgpuRenderer, WgpuSurfaceConfig};
use ndk::native_window::NativeWindow;
use raw_window_handle::{
    AndroidDisplayHandle, AndroidNdkWindowHandle, DisplayHandle, HandleError, HasDisplayHandle,
    HasWindowHandle, RawDisplayHandle, RawWindowHandle, WindowHandle,
};

use super::{AndroidDisplay, CallbackSlot};

/// An `ANativeWindow` as raw-window-handle names it, for wgpu's surface.
#[derive(Clone)]
pub(crate) struct RawWindow(pub NativeWindow);

impl std::fmt::Debug for RawWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RawWindow({:p})", self.0.ptr())
    }
}

impl HasWindowHandle for RawWindow {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let handle = AndroidNdkWindowHandle::new(self.0.ptr().cast::<std::ffi::c_void>());
        // SAFETY: the NativeWindow this holds keeps its ANativeWindow acquired while it lives.
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::AndroidNdk(handle)) })
    }
}

impl HasDisplayHandle for RawWindow {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        // SAFETY: Android's display handle carries nothing.
        Ok(unsafe { DisplayHandle::borrow_raw(RawDisplayHandle::Android(AndroidDisplayHandle::new())) })
    }
}

pub(crate) struct AndroidWindowState {
    pub(crate) handle: AnyWindowHandle,
    app: AndroidApp,
    gpu_context: GpuContext,
    renderer: RefCell<Option<WgpuRenderer>>,
    native: RefCell<Option<RawWindow>>,
    /// The surface's size in device pixels.
    device_size: Cell<Size<DevicePixels>>,
    scale: Cell<f32>,
    visibility: Cell<WindowVisibility>,
    active: Cell<bool>,
    pub(crate) mouse_position: Cell<Point<Pixels>>,
    pub(crate) modifiers: Cell<Modifiers>,
    insets: RefCell<WindowInsets>,
    force_next_frame: Cell<bool>,
    title: RefCell<String>,
    back_enabled: Cell<bool>,
    pub(crate) input_handler: CallbackSlot<PlatformInputHandler>,
    pub(crate) text_input: super::text_input::TextInput,
    request_frame_callback: CallbackSlot<Box<dyn FnMut(RequestFrameOptions)>>,
    pub(crate) input_callback: CallbackSlot<Box<dyn FnMut(PlatformInput) -> DispatchEventResult>>,
    active_status_callback: CallbackSlot<Box<dyn FnMut(bool)>>,
    visibility_callback: CallbackSlot<Box<dyn FnMut(WindowVisibility)>>,
    hover_status_callback: CallbackSlot<Box<dyn FnMut(bool)>>,
    resize_callback: CallbackSlot<Box<dyn FnMut(Size<Pixels>, f32)>>,
    display_changed_callback: CallbackSlot<Box<dyn FnMut()>>,
    moved_callback: CallbackSlot<Box<dyn FnMut()>>,
    should_close_callback: CallbackSlot<Box<dyn FnMut() -> bool>>,
    hit_test_callback: CallbackSlot<Box<dyn FnMut() -> Option<WindowControlArea>>>,
    close_callback: CallbackSlot<Box<dyn FnOnce()>>,
    appearance_changed_callback: CallbackSlot<Box<dyn FnMut()>>,
    insets_changed_callback: CallbackSlot<Box<dyn FnMut(WindowInsets)>>,
    pub(crate) back_callback: CallbackSlot<Box<dyn FnMut()>>,
}

/// The activity's density as GPUI's scale: Android's dp is 1/160 inch.
pub(crate) fn scale_of(app: &AndroidApp) -> f32 {
    app.config().density().map_or(1.0, |dpi| dpi as f32 / 160.0)
}

impl AndroidWindowState {
    fn logical_size(&self) -> Size<Pixels> {
        let s = self.device_size.get();
        let scale = self.scale.get();
        size(px(s.width.0 as f32 / scale), px(s.height.0 as f32 / scale))
    }

    fn surface_config(&self) -> WgpuSurfaceConfig {
        WgpuSurfaceConfig {
            size: self.device_size.get(),
            transparent: false,
            // Fifo blocks in get_current_texture while the surface is being torn down; Mailbox
            // does not, where the driver has it.
            preferred_present_mode: Some(gpui_wgpu::wgpu::PresentMode::Mailbox),
        }
    }

    /// Takes the activity's new surface: makes the renderer on the first, configures it on the
    /// rest. Called from the event loop on `InitWindow`.
    pub(crate) fn attach(&self, window: NativeWindow) -> anyhow::Result<()> {
        let raw = RawWindow(window);
        self.device_size.set(size(DevicePixels(raw.0.width().max(1)), DevicePixels(raw.0.height().max(1))));
        self.scale.set(scale_of(&self.app));
        let config = self.surface_config();
        {
            let mut renderer = self.renderer.borrow_mut();
            match renderer.as_mut() {
                Some(renderer) => {
                    let instance = self
                        .gpu_context
                        .borrow()
                        .as_ref()
                        .map(|c| c.instance.clone())
                        .ok_or_else(|| anyhow::anyhow!("the GPU context is gone"))?;
                    renderer.replace_surface(&raw, config, &instance)?;
                }
                None => *renderer = Some(WgpuRenderer::new(self.gpu_context.clone(), &raw, config, None)?),
            }
        }
        self.native.replace(Some(raw));
        self.force_next_frame.set(true);
        self.set_visibility(WindowVisibility::Visible);
        self.notify_resize();
        Ok(())
    }

    /// Lets go of the surface before `TerminateWindow` returns: the renderer stops using it and
    /// keeps the rest.
    pub(crate) fn detach(&self) {
        if let Some(renderer) = self.renderer.borrow_mut().as_mut() {
            renderer.unconfigure_surface();
        }
        self.native.replace(None);
        self.set_visibility(WindowVisibility::Hidden);
    }

    pub(crate) fn has_surface(&self) -> bool {
        self.native.borrow().is_some()
    }

    /// The surface's size or density changed (`WindowResized`, `ConfigChanged`).
    pub(crate) fn surface_resized(&self) {
        let Some(raw) = self.native.borrow().clone() else { return };
        let new = size(DevicePixels(raw.0.width().max(1)), DevicePixels(raw.0.height().max(1)));
        let scale = scale_of(&self.app);
        if new == self.device_size.get() && scale == self.scale.get() {
            return;
        }
        self.device_size.set(new);
        self.scale.set(scale);
        if let Some(renderer) = self.renderer.borrow_mut().as_mut() {
            renderer.update_drawable_size(new);
        }
        self.force_next_frame.set(true);
        self.notify_resize();
    }

    fn notify_resize(&self) {
        let (logical, scale) = (self.logical_size(), self.scale.get());
        self.resize_callback.with(|callback| callback(logical, scale));
    }

    pub(crate) fn set_visibility(&self, visibility: WindowVisibility) {
        if self.visibility.replace(visibility) != visibility {
            self.visibility_callback.with(|callback| callback(visibility));
        }
    }

    pub(crate) fn set_active(&self, active: bool) {
        if self.active.replace(active) != active {
            self.active_status_callback.with(|callback| callback(active));
        }
    }

    pub(crate) fn set_insets(&self, insets: WindowInsets) {
        if *self.insets.borrow() == insets {
            return;
        }
        self.insets.replace(insets.clone());
        self.insets_changed_callback.with(|callback| callback(insets));
    }

    pub(crate) fn appearance_changed(&self) {
        self.appearance_changed_callback.with(|callback| callback());
    }

    /// One Choreographer frame: GPUI draws if anything changed.
    pub(crate) fn request_frame(&self) {
        if !self.has_surface() {
            return;
        }
        let device_lost = self.renderer.borrow().as_ref().is_some_and(|r| r.device_lost());
        if device_lost {
            let raw = self.native.borrow().clone();
            if let (Some(raw), Some(renderer)) = (raw, self.renderer.borrow_mut().as_mut())
                && let Err(e) = renderer.recover(&raw)
            {
                log::error!("gpui_android: could not recover the GPU device: {e:#}");
                return;
            }
            self.force_next_frame.set(true);
        }
        let needs_redraw = self.renderer.borrow_mut().as_mut().is_some_and(|r| r.needs_redraw());
        let force_render = self.force_next_frame.replace(false) || needs_redraw;
        self.request_frame_callback.with(|callback| {
            callback(RequestFrameOptions { require_presentation: force_render, force_render, ..Default::default() })
        });
    }

    /// The system's back gesture or key: GPUI's handler when it set one and enabled it.
    pub(crate) fn back(&self) -> bool {
        self.back_enabled.get() && self.back_callback.with(|callback| callback()).is_some()
    }

    /// Whether a back press goes to GPUI rather than to the system.
    pub(crate) fn back_callback_set(&self) -> bool {
        self.back_enabled.get() && self.back_callback.is_set()
    }

    pub(crate) fn close(&self) {
        if let Some(callback) = self.close_callback.take() {
            callback();
        }
    }
}

pub(crate) struct AndroidWindow(pub(crate) Rc<AndroidWindowState>);

impl AndroidWindow {
    pub(crate) fn new(
        handle: AnyWindowHandle,
        _params: WindowParams,
        app: AndroidApp,
        gpu_context: GpuContext,
    ) -> anyhow::Result<Self> {
        let state = Rc::new(AndroidWindowState {
            handle,
            scale: Cell::new(scale_of(&app)),
            app: app.clone(),
            gpu_context,
            renderer: RefCell::new(None),
            native: RefCell::new(None),
            device_size: Cell::new(size(DevicePixels(1), DevicePixels(1))),
            visibility: Cell::new(WindowVisibility::Hidden),
            active: Cell::new(false),
            mouse_position: Cell::new(Point::default()),
            modifiers: Cell::new(Modifiers::default()),
            insets: RefCell::new(WindowInsets::default()),
            force_next_frame: Cell::new(true),
            title: RefCell::new(String::new()),
            back_enabled: Cell::new(false),
            input_handler: CallbackSlot::default(),
            text_input: Default::default(),
            request_frame_callback: CallbackSlot::default(),
            input_callback: CallbackSlot::default(),
            active_status_callback: CallbackSlot::default(),
            visibility_callback: CallbackSlot::default(),
            hover_status_callback: CallbackSlot::default(),
            resize_callback: CallbackSlot::default(),
            display_changed_callback: CallbackSlot::default(),
            moved_callback: CallbackSlot::default(),
            should_close_callback: CallbackSlot::default(),
            hit_test_callback: CallbackSlot::default(),
            close_callback: CallbackSlot::default(),
            appearance_changed_callback: CallbackSlot::default(),
            insets_changed_callback: CallbackSlot::default(),
            back_callback: CallbackSlot::default(),
        });
        // The platform starts GPUI once the activity has a surface, so the renderer and its atlas
        // exist before GPUI asks for them.
        if let Some(window) = app.native_window() {
            state.attach(window)?;
        }
        Ok(Self(state))
    }
}

impl HasWindowHandle for AndroidWindow {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let native = self.0.native.borrow();
        let raw = native.as_ref().ok_or(HandleError::Unavailable)?;
        let handle = AndroidNdkWindowHandle::new(raw.0.ptr().cast::<std::ffi::c_void>());
        // SAFETY: the surface is held until `detach`, which runs on this thread.
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::AndroidNdk(handle)) })
    }
}

impl HasDisplayHandle for AndroidWindow {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        // SAFETY: Android's display handle carries nothing.
        Ok(unsafe { DisplayHandle::borrow_raw(RawDisplayHandle::Android(AndroidDisplayHandle::new())) })
    }
}

impl PlatformWindow for AndroidWindow {
    fn bounds(&self) -> Bounds<Pixels> {
        Bounds::new(Point::default(), self.0.logical_size())
    }

    fn is_maximized(&self) -> bool {
        true
    }

    fn window_bounds(&self) -> WindowBounds {
        WindowBounds::Fullscreen(self.bounds())
    }

    fn content_size(&self) -> Size<Pixels> {
        self.0.logical_size()
    }

    fn resize(&mut self, _size: Size<Pixels>) {
        // The activity sizes its surface.
    }

    fn scale_factor(&self) -> f32 {
        self.0.scale.get()
    }

    fn appearance(&self) -> WindowAppearance {
        super::platform::appearance(&self.0.app)
    }

    fn display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        let s = self.0.logical_size();
        Some(Rc::new(AndroidDisplay::new(s.width.into(), s.height.into())))
    }

    fn mouse_position(&self) -> Point<Pixels> {
        self.0.mouse_position.get()
    }

    fn modifiers(&self) -> Modifiers {
        self.0.modifiers.get()
    }

    fn capslock(&self) -> Capslock {
        Capslock::default()
    }

    fn set_input_handler(&mut self, input_handler: PlatformInputHandler) {
        self.0.input_handler.set(input_handler);
    }

    fn take_input_handler(&mut self) -> Option<PlatformInputHandler> {
        self.0.input_handler.take()
    }

    fn text_input_state_changed(&self, change: gpui::TextInputStateChange) {
        self.0.text_input.state_changed(&self.0.app, &self.0.input_handler, change);
    }

    fn set_text_input_configuration(&mut self, configuration: gpui::TextInputConfiguration) {
        self.0.text_input.set_configuration(&self.0.app, configuration);
    }

    fn prompt(
        &self,
        _level: PromptLevel,
        _msg: &str,
        _detail: Option<&str>,
        _answers: &[PromptButton],
    ) -> Option<futures::channel::oneshot::Receiver<usize>> {
        // GPUI draws its own prompt.
        None
    }

    fn activate(&self) {}

    fn is_active(&self) -> bool {
        self.0.active.get()
    }

    fn visibility(&self) -> WindowVisibility {
        self.0.visibility.get()
    }

    fn is_hovered(&self) -> bool {
        false
    }

    fn background_appearance(&self) -> WindowBackgroundAppearance {
        WindowBackgroundAppearance::Opaque
    }

    fn set_title(&mut self, title: &str) {
        self.0.title.replace(title.to_owned());
    }

    fn get_title(&self) -> String {
        self.0.title.borrow().clone()
    }

    fn set_background_appearance(&self, _background: WindowBackgroundAppearance) {}

    fn minimize(&self) {}

    fn zoom(&self) {}

    fn toggle_fullscreen(&self) {}

    fn is_fullscreen(&self) -> bool {
        true
    }

    fn on_request_frame(&self, callback: Box<dyn FnMut(RequestFrameOptions)>) {
        self.0.request_frame_callback.set(callback);
    }

    fn on_input(&self, callback: Box<dyn FnMut(PlatformInput) -> DispatchEventResult>) {
        self.0.input_callback.set(callback);
    }

    fn on_active_status_change(&self, callback: Box<dyn FnMut(bool)>) {
        self.0.active_status_callback.set(callback);
    }

    fn on_visibility_change(&self, callback: Box<dyn FnMut(WindowVisibility)>) {
        self.0.visibility_callback.set(callback);
    }

    fn on_hover_status_change(&self, callback: Box<dyn FnMut(bool)>) {
        self.0.hover_status_callback.set(callback);
    }

    fn on_resize(&self, callback: Box<dyn FnMut(Size<Pixels>, f32)>) {
        self.0.resize_callback.set(callback);
    }

    fn on_moved(&self, callback: Box<dyn FnMut()>) {
        self.0.moved_callback.set(callback);
    }

    fn on_display_changed(&self, callback: Box<dyn FnMut()>) {
        self.0.display_changed_callback.set(callback);
    }

    fn on_should_close(&self, callback: Box<dyn FnMut() -> bool>) {
        self.0.should_close_callback.set(callback);
    }

    fn on_hit_test_window_control(&self, callback: Box<dyn FnMut() -> Option<WindowControlArea>>) {
        self.0.hit_test_callback.set(callback);
    }

    fn on_close(&self, callback: Box<dyn FnOnce()>) {
        self.0.close_callback.set(callback);
    }

    fn on_appearance_changed(&self, callback: Box<dyn FnMut()>) {
        self.0.appearance_changed_callback.set(callback);
    }

    fn draw(&self, scene: &Scene) {
        if !self.0.has_surface() {
            return;
        }
        if let Some(renderer) = self.0.renderer.borrow_mut().as_mut() {
            renderer.draw(scene);
        }
    }

    fn sprite_atlas(&self) -> Arc<dyn PlatformAtlas> {
        match self.0.renderer.borrow().as_ref() {
            Some(renderer) => renderer.sprite_atlas().clone(),
            None => {
                log::warn!("gpui_android: a window opened before the activity had a surface draws nothing");
                Arc::new(HeadlessAtlas::default())
            }
        }
    }

    fn is_subpixel_rendering_supported(&self) -> bool {
        false
    }

    fn gpu_specs(&self) -> Option<GpuSpecs> {
        self.0.renderer.borrow().as_ref().and_then(|r| r.gpu_specs())
    }

    fn update_ime_position(&self, _bounds: Bounds<Pixels>) {}

    fn insets(&self) -> WindowInsets {
        self.0.insets.borrow().clone()
    }

    fn on_insets_changed(&self, callback: Box<dyn FnMut(WindowInsets)>) {
        self.0.insets_changed_callback.set(callback);
    }

    fn set_back_handler(&self, callback: Box<dyn FnMut()>) {
        self.0.back_callback.set(callback);
    }

    fn set_back_enabled(&self, enabled: bool) {
        self.0.back_enabled.set(enabled);
    }

    fn show_soft_keyboard(&self) {
        self.0.app.show_soft_input(true);
    }

    fn hide_soft_keyboard(&self) {
        self.0.app.hide_soft_input(true);
    }
}

impl Drop for AndroidWindow {
    fn drop(&mut self) {
        if let Some(mut renderer) = self.0.renderer.borrow_mut().take() {
            renderer.destroy();
        }
        self.0.native.replace(None);
    }
}
