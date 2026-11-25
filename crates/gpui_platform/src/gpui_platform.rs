//! Convenience crate that re-exports GPUI's platform traits and the
//! `current_platform` constructor so consumers don't need `#[cfg]` gating.

pub use gpui::Platform;

#[cfg(target_family = "wasm")]
pub use gpui_web::WebBackendPreference;

use std::rc::Rc;

/// Returns a background executor for the current platform.
pub fn background_executor() -> gpui::BackgroundExecutor {
    current_platform(true).background_executor()
}

pub fn application() -> gpui::Application {
    gpui::Application::with_platform(current_platform(false))
}

pub fn headless() -> gpui::Application {
    gpui::Application::with_platform(current_platform(true))
}

/// Returns a Linux app that may switch among `allowed_modes`.
///
/// It starts windowed in the process's own environment, or headless if that names no allowed
/// display server. Set another initial mode with [`gpui::Application::with_windowing`], and
/// switch later with [`gpui::App::request_windowing`].
#[cfg(all(
    any(target_os = "linux", target_os = "freebsd"),
    not(target_env = "ohos")
))]
pub fn linux(allowed_modes: gpui::WindowingModes) -> gpui::Application {
    gpui::Application::with_platform(gpui_linux::linux_platform(allowed_modes))
}

#[cfg(target_family = "wasm")]
pub fn application_with_web_backend(backend_preference: WebBackendPreference) -> gpui::Application {
    let platform = Rc::new(gpui_web::WebPlatform::new_with_backend(
        true,
        backend_preference,
    ));
    gpui::Application::with_platform(platform)
}

/// Unlike `application`, this function returns a single-threaded web application.
#[cfg(target_family = "wasm")]
pub fn single_threaded_web() -> gpui::Application {
    let platform = Rc::new(gpui_web::WebPlatform::new(false));
    gpui::Application::with_platform(platform)
}

/// Initializes panic hooks and logging for the web platform.
/// Call this before running the application in a wasm_bindgen entrypoint.
#[cfg(target_family = "wasm")]
pub fn web_init() {
    console_error_panic_hook::set_once();
    gpui_web::init_logging();
}

/// Returns the default [`Platform`] for the current OS.
pub fn current_platform(headless: bool) -> Rc<dyn Platform> {
    cfg_select! {
        target_os = "ios" => gpui_ios::current_platform(headless),
        target_os = "android" => gpui_android::current_platform(headless),
        target_os = "macos" => Rc::new(gpui_macos::MacPlatform::new(headless)),
        target_os = "windows" => Rc::new(
            gpui_windows::WindowsPlatform::new(headless)
                .expect("failed to initialize Windows platform"),
        ),
        all(
            any(target_os = "linux", target_os = "freebsd"),
            not(target_env = "ohos")
        ) => gpui_linux::current_platform(headless),
        target_family = "wasm" => {
            let _ = headless;
            Rc::new(gpui_web::WebPlatform::new_with_backend(
                true,
                gpui_web::WebBackendPreference::Auto,
            ))
        },
    }
}

/// Returns the current platform's text system for benchmark contexts, built once per
/// thread and shared by every context on it.
///
/// `#[gpui::bench]` creates a context for every Criterion routine call. Building a whole
/// platform each time only to take its text system leaks the platform's threads on Linux
/// (a 20-worker dispatcher and a timer thread per call), which slows every later
/// benchmark in the process.
#[cfg(feature = "bench-support")]
pub fn bench_text_system() -> std::sync::Arc<dyn gpui::PlatformTextSystem> {
    thread_local! {
        static TEXT_SYSTEM: std::sync::Arc<dyn gpui::PlatformTextSystem> =
            current_platform(true).text_system();
    }
    TEXT_SYSTEM.with(|text_system| text_system.clone())
}

/// Returns a new [`HeadlessRenderer`] for the current platform, if available.
#[cfg(any(feature = "bench-support", feature = "test-support"))]
pub fn current_headless_renderer() -> anyhow::Result<Option<Box<dyn gpui::PlatformHeadlessRenderer>>>
{
    #[cfg(target_os = "macos")]
    {
        Ok(Some(Box::new(
            gpui_macos::metal_renderer::MetalHeadlessRenderer::new()?,
        )))
    }

    #[cfg(target_os = "linux")]
    {
        gpui_wgpu::WgpuHeadlessRenderer::new()
            .map(|renderer| Some(Box::new(renderer) as Box<dyn gpui::PlatformHeadlessRenderer>))
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Ok(None)
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use gpui::{AppContext, Empty, VisualTestAppContext};
    use std::cell::RefCell;
    use std::time::Duration;

    // Note: All VisualTestAppContext tests are ignored by default because they require
    // the macOS main thread. Standard Rust tests run on worker threads, which causes
    // SIGABRT when interacting with macOS AppKit/Cocoa APIs.
    //
    // To run these tests, use:
    // cargo test -p gpui visual_test_context -- --ignored --test-threads=1

    #[test]
    #[ignore] // Requires macOS main thread
    fn test_foreground_tasks_run_with_run_until_parked() {
        let mut cx = VisualTestAppContext::new(current_platform(false));

        let task_ran = Rc::new(RefCell::new(false));

        // Spawn a foreground task via the App's spawn method
        // This should use our TestDispatcher, not the MacDispatcher
        {
            let task_ran = task_ran.clone();
            cx.update(|cx| {
                cx.spawn(async move |_| {
                    *task_ran.borrow_mut() = true;
                })
                .detach();
            });
        }

        // The task should not have run yet
        assert!(!*task_ran.borrow());

        // Run until parked should execute the foreground task
        cx.run_until_parked();

        // Now the task should have run
        assert!(*task_ran.borrow());
    }

    #[test]
    #[ignore] // Requires macOS main thread
    fn test_advance_clock_triggers_delayed_tasks() {
        let mut cx = VisualTestAppContext::new(current_platform(false));

        let task_ran = Rc::new(RefCell::new(false));

        // Spawn a task that waits for a timer
        {
            let task_ran = task_ran.clone();
            let executor = cx.background_executor.clone();
            cx.update(|cx| {
                cx.spawn(async move |_| {
                    executor.timer(Duration::from_millis(500)).await;
                    *task_ran.borrow_mut() = true;
                })
                .detach();
            });
        }

        // Run until parked - the task should be waiting on the timer
        cx.run_until_parked();
        assert!(!*task_ran.borrow());

        // Advance clock past the timer duration
        cx.advance_clock(Duration::from_millis(600));

        // Now the task should have completed
        assert!(*task_ran.borrow());
    }

    #[test]
    #[ignore] // Requires macOS main thread - window creation fails on test threads
    fn test_window_spawn_uses_test_dispatcher() {
        let mut cx = VisualTestAppContext::new(current_platform(false));

        let task_ran = Rc::new(RefCell::new(false));

        let window = cx
            .open_offscreen_window_default(|_, cx| cx.new(|_| Empty))
            .expect("Failed to open window");

        // Spawn a task via window.spawn - this is the critical test case
        // for tooltip behavior, as tooltips use window.spawn for delayed show
        {
            let task_ran = task_ran.clone();
            cx.update_window(window.into(), |_, window, cx| {
                window
                    .spawn(cx, async move |_| {
                        *task_ran.borrow_mut() = true;
                    })
                    .detach();
            })
            .ok();
        }

        // The task should not have run yet
        assert!(!*task_ran.borrow());

        // Run until parked should execute the foreground task spawned via window
        cx.run_until_parked();

        // Now the task should have run
        assert!(*task_ran.borrow());
    }
}

/// Elements drawn under transformations and filters, through each renderer this platform has,
/// read back as pixels.
#[cfg(all(test, target_os = "macos", feature = "test-support"))]
mod effect_tests {
    use gpui::{
        AnyElement, AppContext as _, Context, HeadlessAppContext, IntoElement, ParentElement as _,
        PlatformHeadlessRenderer, Render, RenderImage, Styled as _, Transformation, Window, div,
        img, point, px, radians, rgb, size,
    };
    use std::sync::Arc;

    type Renderer = fn() -> anyhow::Result<Option<Box<dyn PlatformHeadlessRenderer>>>;

    fn metal() -> anyhow::Result<Option<Box<dyn PlatformHeadlessRenderer>>> {
        Ok(Some(Box::new(
            gpui_macos::metal_renderer::MetalHeadlessRenderer::new()?,
        )))
    }

    fn wgpu() -> anyhow::Result<Option<Box<dyn PlatformHeadlessRenderer>>> {
        Ok(Some(Box::new(gpui_wgpu::WgpuHeadlessRenderer::new()?)))
    }

    const RENDERERS: [(&str, Renderer); 2] = [("metal", metal), ("wgpu", wgpu)];

    struct Scene(fn() -> AnyElement);

    impl Render for Scene {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size(px(64.)).bg(rgb(0x000000)).child((self.0)())
        }
    }

    /// The 64-by-64 scene drawn by `renderer`, and its pixels per logical pixel.
    fn draw(renderer: Renderer, scene: fn() -> AnyElement) -> (image::RgbaImage, u32) {
        let mut cx = HeadlessAppContext::with_platform(
            Arc::new(gpui_apple::AppleTextSystem::new()),
            Arc::new(()),
            renderer,
        );
        let window = cx
            .open_window(size(px(64.), px(64.)), |_, cx| cx.new(|_| Scene(scene)))
            .expect("a headless window");
        cx.run_until_parked();
        let image = cx.capture_screenshot(window.into()).expect("a screenshot");
        let scale = image.width() / 64;
        (image, scale)
    }

    /// Whether the pixel at logical `(x, y)` is mostly red.
    fn is_red(image: &image::RgbaImage, scale: u32, x: f32, y: f32) -> bool {
        let pixel = image
            .get_pixel((x * scale as f32) as u32, (y * scale as f32) as u32)
            .0;
        pixel[0] > 200 && pixel[1] < 60 && pixel[2] < 60
    }

    fn red_box() -> gpui::Div {
        div().absolute().bg(rgb(0xff0000))
    }

    #[test]
    fn a_scaled_box_is_drawn_twice_as_large() {
        for (name, renderer) in RENDERERS {
            // 16 wide at 16, scaled twice about its center: 8..40.
            let (image, scale) = draw(renderer, || {
                red_box()
                    .left(px(16.))
                    .top(px(16.))
                    .size(px(16.))
                    .transform(Transformation::scale(size(2., 2.)))
                    .into_any_element()
            });
            for (x, y, red) in [
                (9., 9., true),
                (39., 24., true),
                (24., 39., true),
                (6., 24., false),
                (42., 42., false),
            ] {
                assert_eq!(is_red(&image, scale, x, y), red, "{name} at ({x}, {y})");
            }
        }
    }

    #[test]
    fn a_rotated_box_is_drawn_as_a_diamond() {
        for (name, renderer) in RENDERERS {
            // 20 wide at 22, turned 45 degrees about (32, 32): a diamond reaching 14.1.
            let (image, scale) = draw(renderer, || {
                red_box()
                    .left(px(22.))
                    .top(px(22.))
                    .size(px(20.))
                    .transform(Transformation::rotate(radians(std::f32::consts::FRAC_PI_4)))
                    .into_any_element()
            });
            for (x, y, red) in [
                (32., 32., true),
                (32., 19.5, true),
                (44.5, 32., true),
                (23.5, 23.5, false),
                (40.5, 40.5, false),
            ] {
                assert_eq!(is_red(&image, scale, x, y), red, "{name} at ({x}, {y})");
            }
        }
    }

    #[test]
    fn a_moved_clip_cuts_its_children_where_it_is_drawn() {
        for (name, renderer) in RENDERERS {
            // A 32-wide clip of a 64-wide red fill, at 8, moved by 16: it shows 24..56.
            let (image, scale) = draw(renderer, || {
                div()
                    .absolute()
                    .left(px(8.))
                    .top(px(8.))
                    .size(px(32.))
                    .overflow_hidden()
                    .transform(Transformation::translate(point(px(16.), px(16.))))
                    .child(div().size(px(64.)).bg(rgb(0xff0000)))
                    .into_any_element()
            });
            for (x, y, red) in [
                (40., 40., true),
                (26., 26., true),
                (54., 54., true),
                (58., 40., false),
                (40., 58., false),
                (12., 12., false),
            ] {
                assert_eq!(is_red(&image, scale, x, y), red, "{name} at ({x}, {y})");
            }
        }
    }

    #[test]
    fn rotated_text_turns_with_its_box() {
        for (name, renderer) in RENDERERS {
            fn text(turn: f32) -> impl IntoElement {
                div()
                    .absolute()
                    .left(px(4.))
                    .top(px(24.))
                    .w(px(56.))
                    .h(px(16.))
                    .text_size(px(14.))
                    .text_color(rgb(0xff0000))
                    .child("IIIIIIIIIIII")
                    .transform(Transformation::rotate(radians(turn)))
            }
            let (flat, scale) = draw(renderer, || text(0.).into_any_element());
            let (turned, _) = draw(renderer, || {
                text(std::f32::consts::FRAC_PI_2).into_any_element()
            });
            let extent = |image: &image::RgbaImage| {
                let mut min = (u32::MAX, u32::MAX);
                let mut max = (0, 0);
                for (x, y, pixel) in image.enumerate_pixels() {
                    if pixel.0[0] > 128 {
                        min = (min.0.min(x), min.1.min(y));
                        max = (max.0.max(x), max.1.max(y));
                    }
                }
                assert!(max.0 >= min.0, "{name}: no text drawn");
                ((max.0 - min.0) / scale, (max.1 - min.1) / scale)
            };
            let (flat_width, flat_height) = extent(&flat);
            let (turned_width, turned_height) = extent(&turned);
            assert!(
                flat_width > flat_height * 2,
                "{name}: flat {flat_width}x{flat_height}"
            );
            assert!(
                turned_height > turned_width * 2,
                "{name}: turned {turned_width}x{turned_height}"
            );
        }
    }

    /// The color at logical `(x, y)`.
    fn rgb_at(image: &image::RgbaImage, scale: u32, x: f32, y: f32) -> [i32; 3] {
        let pixel = image
            .get_pixel((x * scale as f32) as u32, (y * scale as f32) as u32)
            .0;
        [pixel[0] as i32, pixel[1] as i32, pixel[2] as i32]
    }

    fn assert_near(name: &str, actual: [i32; 3], expected: [i32; 3]) {
        assert!(
            actual
                .iter()
                .zip(expected)
                .all(|(a, e)| (a - e).abs() <= 12),
            "{name}: {actual:?}, expected about {expected:?}"
        );
    }

    #[test]
    fn a_grayscaled_parent_grays_its_children() {
        for (name, renderer) in RENDERERS {
            let (image, scale) = draw(renderer, || {
                div()
                    .absolute()
                    .left(px(16.))
                    .top(px(16.))
                    .size(px(32.))
                    .filter_grayscale(1.)
                    .child(div().size_full().bg(rgb(0xff0000)))
                    .into_any_element()
            });
            // Red's luminance, 0.2126 of full.
            assert_near(name, rgb_at(&image, scale, 32., 32.), [54, 54, 54]);
            assert_near(name, rgb_at(&image, scale, 4., 4.), [0, 0, 0]);
        }
    }

    #[test]
    fn brightness_scales_a_box_and_its_border() {
        for (name, renderer) in RENDERERS {
            let (image, scale) = draw(renderer, || {
                red_box()
                    .left(px(16.))
                    .top(px(16.))
                    .size(px(32.))
                    .border_4()
                    .border_color(rgb(0x00ff00))
                    .filter_brightness(0.5)
                    .into_any_element()
            });
            assert_near(name, rgb_at(&image, scale, 32., 32.), [128, 0, 0]);
            assert_near(name, rgb_at(&image, scale, 18., 32.), [0, 128, 0]);
        }
    }

    #[test]
    fn an_image_takes_its_ancestors_filters() {
        for (name, renderer) in RENDERERS {
            let (image, scale) = draw(renderer, || {
                // 8 by 8 of opaque red, in the BGRA order images are kept in.
                let red = image::RgbaImage::from_fn(8, 8, |_, _| image::Rgba([0, 0, 255, 255]));
                let red = Arc::new(RenderImage::new([image::Frame::new(red)]));
                div()
                    .absolute()
                    .left(px(16.))
                    .top(px(16.))
                    .size(px(32.))
                    .filter_grayscale(1.)
                    .filter_brightness(2.)
                    .child(img(red).size_full())
                    .into_any_element()
            });
            // Gray at red's luminance, doubled.
            assert_near(name, rgb_at(&image, scale, 32., 32.), [108, 108, 108]);
        }
    }
}
