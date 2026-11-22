//! GPUI's `Platform` on an Android `GameActivity`.
//!
//! `run` is the activity's event loop on the thread `android_main` runs on: it waits in
//! `poll_events`, woken by the activity, by GPUI's dispatcher and by Choreographer's frame
//! callbacks; handles the activity's lifecycle; drains GPUI's main-thread queue; and draws a frame
//! on each vsync while a window has a surface. GPUI is started once the activity has its first
//! surface, so a window's renderer exists before GPUI draws. Windows after the first are on
//! surfaces the app hands over (others.rs).

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use android_activity::{AndroidApp, AndroidAppWaker, MainEvent, PollEvent};
use anyhow::anyhow;
use futures::channel::oneshot;
use gpui::{
    Action, AnyWindowHandle, AppLifecyclePhase, BackgroundExecutor, ClipboardItem, CursorStyle, DevicePixels,
    DummyKeyboardMapper, ForegroundExecutor, Keymap, Menu, MenuItem, PathPromptOptions, Platform,
    PlatformDisplay, PlatformKeyboardLayout, PlatformKeyboardMapper, PlatformTextSystem,
    PlatformWindow, PriorityQueueReceiver, Result, RunnableVariant, Task, ThermalState,
    Point, WindowAppearance, WindowParams, point,
};
use gpui_wgpu::{CosmicTextSystem, GpuContext};

use super::window::{AndroidWindowState, scale_of};
use super::others::{self, Told};
use super::{AndroidDisplay, AndroidWindow, CallbackSlot, dispatcher, input};

/// Set by Choreographer's frame callback, read by the event loop after the wake it sends.
static FRAME_DUE: AtomicBool = AtomicBool::new(false);
/// Whether a frame callback is posted and has not run.
static FRAME_POSTED: AtomicBool = AtomicBool::new(false);
static WAKER: OnceLock<AndroidAppWaker> = OnceLock::new();

unsafe extern "C" fn on_frame(_frame_time_nanos: std::os::raw::c_long, _data: *mut std::ffi::c_void) {
    FRAME_POSTED.store(false, Ordering::SeqCst);
    FRAME_DUE.store(true, Ordering::SeqCst);
    if let Some(waker) = WAKER.get() {
        waker.wake();
    }
}

/// Wakes the event loop from any thread: something is queued for GPUI's thread (others.rs).
pub(crate) fn wake() {
    if let Some(waker) = WAKER.get() {
        waker.wake();
    }
}

/// Asks Choreographer for the next vsync, once. On the looper thread, whose looper it posts to.
fn post_frame() {
    if FRAME_POSTED.swap(true, Ordering::SeqCst) {
        return;
    }
    // SAFETY: called on the activity's looper thread, which has a looper; `on_frame` touches only
    // statics.
    unsafe {
        let choreographer = ndk_sys::AChoreographer_getInstance();
        if choreographer.is_null() {
            FRAME_POSTED.store(false, Ordering::SeqCst);
            return;
        }
        ndk_sys::AChoreographer_postFrameCallback(choreographer, Some(on_frame), std::ptr::null_mut());
    }
}

/// Whether this is the Android emulator. Its Vulkan, passed through to the host (SwiftShader, or
/// MoltenVK on a Mac), hangs building GPUI's pipelines or cannot compile its shaders.
fn is_emulator() -> bool {
    ["ro.boot.qemu", "ro.kernel.qemu"].iter().any(|name| {
        let name = std::ffi::CString::new(*name).expect("no nul");
        let mut value = [0 as std::ffi::c_char; 92];
        // SAFETY: `value` is PROP_VALUE_MAX (92) bytes, as the call requires.
        let len = unsafe { libc::__system_property_get(name.as_ptr(), value.as_mut_ptr()) };
        len == 1 && value[0] == b'1' as std::ffi::c_char
    })
}

/// The system's light or dark mode.
pub(crate) fn appearance(app: &AndroidApp) -> WindowAppearance {
    match app.config().ui_mode_night() {
        ndk::configuration::UiModeNight::Yes => WindowAppearance::Dark,
        _ => WindowAppearance::Light,
    }
}

struct AndroidKeyboardLayout;

impl PlatformKeyboardLayout for AndroidKeyboardLayout {
    fn id(&self) -> &str {
        "android-default"
    }

    fn name(&self) -> &str {
        "Android Default"
    }
}

pub struct AndroidPlatform(Rc<AndroidPlatformState>);

pub(crate) struct AndroidPlatformState {
    app: AndroidApp,
    main_queue: RefCell<PriorityQueueReceiver<RunnableVariant>>,
    background_executor: BackgroundExecutor,
    foreground_executor: ForegroundExecutor,
    text_system: Arc<CosmicTextSystem>,
    gpu_context: GpuContext,
    window: RefCell<Weak<AndroidWindowState>>,
    /// The windows beyond the activity's own, by id, each on a surface the app hands over.
    others: RefCell<Vec<(u64, Weak<AndroidWindowState>)>>,
    next_other: Cell<u64>,
    /// The activity's own surface's top left on the screen, as last told: kept for its window,
    /// which may open after the app first tells it.
    main_origin: Cell<Point<DevicePixels>>,
    destroyed: Cell<bool>,
    input_available: Cell<bool>,
    appearance: Cell<WindowAppearance>,
    /// The item GPUI last wrote, kept for its metadata: the system clipboard holds text alone.
    clipboard: RefCell<Option<ClipboardItem>>,
    quit_callback: CallbackSlot<Box<dyn FnMut() -> bool>>,
    open_urls_callback: CallbackSlot<Box<dyn FnMut(Vec<String>)>>,
    lifecycle_callback: CallbackSlot<Box<dyn FnMut(AppLifecyclePhase)>>,
    memory_warning_callback: CallbackSlot<Box<dyn FnMut()>>,
    displays_changed_callback: CallbackSlot<Box<dyn FnMut()>>,
}

impl AndroidPlatform {
    /// Made on the thread `android_main` runs on, which becomes GPUI's main thread.
    pub fn new(app: AndroidApp) -> Self {
        if is_emulator() && std::env::var_os("WGPU_BACKEND").is_none() {
            log::info!("gpui_android: an emulator; drawing with GL, as its Vulkan hangs or fails");
            // SAFETY: GPUI has not started; no other thread reads the environment yet.
            unsafe { std::env::set_var("WGPU_BACKEND", "gl") };
        }
        let waker = app.create_waker();
        let _ = WAKER.set(waker.clone());
        let (dispatcher, main_queue) = dispatcher::AndroidDispatcher::new(waker);
        let dispatcher = Arc::new(dispatcher);
        let text_system = Arc::new(CosmicTextSystem::new_without_system_fonts("Roboto"));
        // fontdb knows no font folder on Android.
        text_system.load_fonts_dir("/system/fonts");
        let appearance = appearance(&app);
        Self(Rc::new(AndroidPlatformState {
            app,
            main_queue: RefCell::new(main_queue),
            background_executor: BackgroundExecutor::new(dispatcher.clone()),
            foreground_executor: ForegroundExecutor::new(dispatcher),
            text_system,
            gpu_context: GpuContext::default(),
            window: RefCell::new(Weak::new()),
            others: RefCell::new(Vec::new()),
            next_other: Cell::new(1),
            main_origin: Cell::new(Point::default()),
            destroyed: Cell::new(false),
            input_available: Cell::new(false),
            appearance: Cell::new(appearance),
            clipboard: RefCell::new(None),
            quit_callback: CallbackSlot::default(),
            open_urls_callback: CallbackSlot::default(),
            lifecycle_callback: CallbackSlot::default(),
            memory_warning_callback: CallbackSlot::default(),
            displays_changed_callback: CallbackSlot::default(),
        }))
    }
}

impl AndroidPlatformState {
    fn window(&self) -> Option<Rc<AndroidWindowState>> {
        self.window.borrow().upgrade()
    }

    /// Every window: the activity's own first, then the others still open.
    fn windows(&self) -> Vec<Rc<AndroidWindowState>> {
        let mut others = self.others.borrow_mut();
        others.retain(|(_, w)| w.strong_count() > 0);
        self.window().into_iter().chain(others.iter().filter_map(|(_, w)| w.upgrade())).collect()
    }

    fn other(&self, id: u64) -> Option<Rc<AndroidWindowState>> {
        self.others.borrow().iter().find(|(i, _)| *i == id).and_then(|(_, w)| w.upgrade())
    }

    /// What an app's activity told of a window beyond the activity's own (others.rs).
    fn told(&self, told: Told) {
        let scale = scale_of(&self.app);
        match told {
            Told::Surface(id, native) => {
                if let Some(window) = self.other(id)
                    && let Err(e) = window.attach(native)
                {
                    log::error!("gpui_android: could not draw on window {id}'s surface: {e:#}");
                }
            }
            Told::Gone(id, done) => {
                if let Some(window) = self.other(id) {
                    window.detach();
                }
                let _ = done.send(());
            }
            Told::Resized(id) => {
                if let Some(window) = self.other(id) {
                    window.surface_resized();
                }
            }
            Told::Touch { id, pointer, phase, x, y, pressure } => {
                if let Some(window) = self.other(id) {
                    input::touch(&window, pointer, phase, x, y, pressure, scale, input::Contact::FINGER);
                }
            }
            Told::Insets(id, safe, ime) => {
                if let Some(window) = self.other(id) {
                    window.set_insets(others::as_insets(safe, ime, scale));
                }
            }
            Told::Placed(id, x, y) => {
                let origin = point(DevicePixels(x), DevicePixels(y));
                if id == 0 {
                    self.main_origin.set(origin);
                }
                let window = if id == 0 { self.window() } else { self.other(id) };
                if let Some(window) = window {
                    window.set_origin(origin);
                }
            }
            Told::Focus(id, focused) => {
                if let Some(window) = self.other(id) {
                    window.set_active(focused);
                }
            }
            Told::Ended(id) => {
                if let Some(window) = self.other(id) {
                    window.detach();
                    window.end();
                }
            }
            Told::Key { id, code, meta, down, repeat, typed, bare } => {
                if let Some(window) = self.other(id) {
                    input::told_key(&window, code, meta, down, repeat, typed, bare);
                }
            }
            Told::Text(id, state) => {
                if let Some(window) = self.other(id) {
                    window.text_input.apply(&state, &window.input_handler);
                }
            }
            Told::Action(id) => {
                if let Some(window) = self.other(id) {
                    input::action(&window);
                }
            }
        }
    }

    fn read_insets(&self, window: &AndroidWindowState) {
        if let Some(insets) = super::insets::read(&self.app, scale_of(&self.app)) {
            window.set_insets(insets);
        }
    }

    fn lifecycle(&self, phase: AppLifecyclePhase) {
        self.lifecycle_callback.with(|callback| callback(phase));
    }

    /// One event from the activity, inside `poll_events`: a surface must be let go of before its
    /// `TerminateWindow` returns.
    fn handle(&self, event: MainEvent<'_>) {
        match event {
            MainEvent::InitWindow { .. } => {
                if let (Some(window), Some(native)) = (self.window(), self.app.native_window())
                    && let Err(e) = window.attach(native)
                {
                    log::error!("gpui_android: could not draw on the activity's surface: {e:#}");
                }
                if let Some(window) = self.window() {
                    self.read_insets(&window);
                }
            }
            MainEvent::TerminateWindow { .. } => {
                if let Some(window) = self.window() {
                    window.detach();
                }
            }
            MainEvent::WindowResized { .. } | MainEvent::ContentRectChanged { .. } => {
                if let Some(window) = self.window() {
                    window.surface_resized();
                    self.read_insets(&window);
                }
            }
            MainEvent::InsetsChanged { .. } => {
                if let Some(window) = self.window() {
                    self.read_insets(&window);
                }
            }
            MainEvent::RedrawNeeded { .. } => {
                if let Some(window) = self.window() {
                    window.request_frame();
                }
            }
            MainEvent::ConfigChanged { .. } => {
                let appearance = appearance(&self.app);
                if let Some(window) = self.window() {
                    window.surface_resized();
                    if self.appearance.replace(appearance) != appearance {
                        window.appearance_changed();
                    }
                }
                self.displays_changed_callback.with(|callback| callback());
            }
            MainEvent::GainedFocus => {
                if let Some(window) = self.window() {
                    window.set_active(true);
                }
                self.lifecycle(AppLifecyclePhase::Active);
            }
            MainEvent::LostFocus => {
                if let Some(window) = self.window() {
                    window.set_active(false);
                }
                self.lifecycle(AppLifecyclePhase::Inactive);
            }
            MainEvent::Start => self.lifecycle(AppLifecyclePhase::Foreground),
            MainEvent::Resume { .. } => self.lifecycle(AppLifecyclePhase::Foreground),
            MainEvent::Pause => self.lifecycle(AppLifecyclePhase::Inactive),
            MainEvent::Stop => self.lifecycle(AppLifecyclePhase::Background),
            MainEvent::LowMemory => {
                self.memory_warning_callback.with(|callback| callback());
            }
            MainEvent::InputAvailable => self.input_available.set(true),
            MainEvent::Destroy => {
                self.quit_callback.with(|callback| callback());
                for window in self.windows() {
                    window.close();
                }
                self.destroyed.set(true);
            }
            _ => {}
        }
    }
}

impl Platform for AndroidPlatform {
    fn platform_info(&self) -> gpui::PlatformInfo {
        gpui::PlatformInfo { os: gpui::OperatingSystem::Android, target: gpui::PlatformTarget::Native }
    }

    fn background_executor(&self) -> BackgroundExecutor {
        self.0.background_executor.clone()
    }

    fn foreground_executor(&self) -> ForegroundExecutor {
        self.0.foreground_executor.clone()
    }

    fn text_system(&self) -> Arc<dyn PlatformTextSystem> {
        self.0.text_system.clone()
    }

    fn run(&self, on_finish_launching: Box<dyn 'static + FnOnce()>) {
        let state = self.0.clone();
        let app = state.app.clone();
        let mut launch = Some(on_finish_launching);
        while !state.destroyed.get() {
            app.poll_events(None, |event| {
                if let PollEvent::Main(event) = event {
                    state.handle(event);
                }
            });
            if launch.is_some() && app.native_window().is_some() {
                // GPUI opens its windows here, once there is a surface to draw on.
                (launch.take().expect("checked"))();
            }
            if state.input_available.replace(false)
                && let Some(window) = state.window()
            {
                input::drain(&app, &window, scale_of(&app));
            } else if state.input_available.get() {
                state.input_available.set(false);
            }
            for told in others::drain() {
                state.told(told);
            }
            dispatcher::drain(&mut state.main_queue.borrow_mut());
            let windows = state.windows();
            if FRAME_DUE.swap(false, Ordering::SeqCst) {
                for window in &windows {
                    window.request_frame();
                }
            }
            if windows.iter().any(|w| w.has_surface()) {
                post_frame();
            }
        }
        // Whatever GPUI queued as it closed.
        dispatcher::drain(&mut state.main_queue.borrow_mut());
        // GPUI cannot start again on the next activity's thread (see `set_android_app`). `_exit`, not
        // `exit`: the C++ glue's static destructors would run on this thread with the main thread's
        // JNIEnv, which CheckJNI aborts on, while the main thread waits in `onDestroy` (an ANR).
        // SAFETY: ends the process; nothing after it runs.
        unsafe { libc::_exit(0) };
    }

    fn quit(&self) {
        // An activity finishes itself through its Java side; GameActivity's is not reached yet.
        log::warn!("gpui_android: an app cannot quit itself; the system ends it");
    }

    fn restart(&self, _binary_path: Option<PathBuf>, _arguments: Vec<std::ffi::OsString>) {
        log::warn!("gpui_android: an app cannot restart itself");
    }

    fn activate(&self, _ignoring_other_apps: bool) {}

    fn hide(&self) {}

    fn hide_other_apps(&self) {}

    fn unhide_other_apps(&self) {}

    fn displays(&self) -> Vec<Rc<dyn PlatformDisplay>> {
        self.primary_display().into_iter().collect()
    }

    fn primary_display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        let window = self.0.app.native_window()?;
        let scale = scale_of(&self.0.app);
        Some(Rc::new(AndroidDisplay::new(window.width() as f32 / scale, window.height() as f32 / scale)))
    }

    fn on_displays_changed(&self, callback: Box<dyn FnMut()>) {
        self.0.displays_changed_callback.set(callback);
    }

    fn active_window(&self) -> Option<AnyWindowHandle> {
        self.0.window().map(|w| w.handle)
    }

    fn open_window(
        &self,
        handle: AnyWindowHandle,
        options: WindowParams,
    ) -> anyhow::Result<Box<dyn PlatformWindow>> {
        // The activity's own surface is the first window; each after it is the app's to show
        // (others.rs), on a surface it hands over.
        if self.0.window().is_some() {
            let id = self.0.next_other.get();
            self.0.next_other.set(id + 1);
            let window = AndroidWindow::new(handle, options, self.0.app.clone(), self.0.gpu_context.clone(), Some(id))?;
            self.0.others.borrow_mut().push((id, Rc::downgrade(&window.0)));
            others::opened(id);
            return Ok(Box::new(window));
        }
        let window = AndroidWindow::new(handle, options, self.0.app.clone(), self.0.gpu_context.clone(), None)?;
        window.0.set_origin(self.0.main_origin.get());
        self.0.window.replace(Rc::downgrade(&window.0));
        Ok(Box::new(window))
    }

    fn window_appearance(&self) -> WindowAppearance {
        self.0.appearance.get()
    }

    fn open_url(&self, url: &str) {
        log::warn!("gpui_android: opening {url} needs the activity's Java side, which is not reached yet");
    }

    fn on_open_urls(&self, callback: Box<dyn FnMut(Vec<String>)>) {
        self.0.open_urls_callback.set(callback);
    }

    fn register_url_scheme(&self, _url: &str) -> Task<Result<()>> {
        // An app's URL schemes are its manifest's intent filters.
        Task::ready(Ok(()))
    }

    fn prompt_for_paths(
        &self,
        _options: PathPromptOptions,
    ) -> oneshot::Receiver<Result<Option<Vec<PathBuf>>>> {
        let (tx, rx) = oneshot::channel();
        let _ = tx.send(Err(anyhow!("gpui_android has no file picker")));
        rx
    }

    fn prompt_for_new_path(
        &self,
        _directory: &Path,
        _suggested_name: Option<&str>,
    ) -> oneshot::Receiver<Result<Option<PathBuf>>> {
        let (tx, rx) = oneshot::channel();
        let _ = tx.send(Err(anyhow!("gpui_android has no save dialog")));
        rx
    }

    fn can_select_mixed_files_and_dirs(&self) -> bool {
        false
    }

    fn reveal_path(&self, _path: &Path) {}

    fn open_with_system(&self, _path: &Path) {}

    fn on_quit(&self, callback: Box<dyn FnMut() -> bool>) {
        self.0.quit_callback.set(callback);
    }

    fn on_reopen(&self, _callback: Box<dyn FnMut()>) {}

    fn on_system_sleep(&self, _callback: Box<dyn FnMut()>) {}

    fn on_system_wake(&self, _callback: Box<dyn FnMut()>) {}

    fn on_app_lifecycle(&self, callback: Box<dyn FnMut(AppLifecyclePhase)>) {
        self.0.lifecycle_callback.set(callback);
    }

    fn on_memory_warning(&self, callback: Box<dyn FnMut()>) {
        self.0.memory_warning_callback.set(callback);
    }

    fn set_menus(&self, _menus: Vec<Menu>, _keymap: &Keymap) {}

    fn set_dock_menu(&self, _menu: Vec<MenuItem>, _keymap: &Keymap) {}

    fn on_app_menu_action(&self, _callback: Box<dyn FnMut(&dyn Action)>) {}

    fn on_will_open_app_menu(&self, _callback: Box<dyn FnMut()>) {}

    fn on_validate_app_menu_command(&self, _callback: Box<dyn FnMut(&dyn Action) -> bool>) {}

    fn thermal_state(&self) -> ThermalState {
        ThermalState::Nominal
    }

    fn on_thermal_state_change(&self, _callback: Box<dyn FnMut()>) {}

    fn prevent_idle_sleep(&self, _reason: &str) -> Task<Result<gpui::ActivityGuard>> {
        Task::ready(Err(anyhow!("gpui_android cannot keep the device awake yet")))
    }

    fn app_path(&self) -> Result<PathBuf> {
        self.0.app.internal_data_path().ok_or_else(|| anyhow!("the activity has no data folder"))
    }

    fn path_for_auxiliary_executable(&self, name: &str) -> Result<PathBuf> {
        Err(anyhow!("an Android app has no auxiliary executable {name}"))
    }

    fn set_cursor_style(&self, _style: CursorStyle) {}

    fn hide_cursor_until_mouse_moves(&self) {}

    fn is_cursor_visible(&self) -> bool {
        false
    }

    fn should_auto_hide_scrollbars(&self) -> bool {
        true
    }

    fn read_from_clipboard(&self) -> Option<ClipboardItem> {
        super::clipboard::item(&self.0.app, self.0.clipboard.borrow().as_ref())
    }

    fn write_to_clipboard(&self, item: ClipboardItem) {
        if let Some(text) = item.text() {
            super::clipboard::write(&self.0.app, &text);
        }
        self.0.clipboard.replace(Some(item));
    }

    fn write_credentials(&self, _url: &str, _username: &str, _password: &[u8]) -> Task<Result<()>> {
        Task::ready(Err(anyhow!("gpui_android has no credential store yet")))
    }

    fn read_credentials(&self, _url: &str) -> Task<Result<Option<(String, Vec<u8>)>>> {
        Task::ready(Ok(None))
    }

    fn delete_credentials(&self, _url: &str) -> Task<Result<()>> {
        Task::ready(Ok(()))
    }

    fn keyboard_layout(&self) -> Box<dyn PlatformKeyboardLayout> {
        Box::new(AndroidKeyboardLayout)
    }

    fn keyboard_mapper(&self) -> Rc<dyn PlatformKeyboardMapper> {
        Rc::new(DummyKeyboardMapper)
    }

    fn on_keyboard_layout_change(&self, _callback: Box<dyn FnMut()>) {}
}
