//! Native Android platform support for GPUI.
//!
//! This crate connects GPUI to the app's own Android `Activity`: each window is a `SurfaceView` of the
//! app's, drawn through `gpui_wgpu` on Vulkan (GL when Vulkan is missing); GPUI runs on a thread of
//! its own with a looper (`start`); the activity's lifecycle and each view's surface, input and
//! keyboard reach it through one queue (`windows`). Text is shaped by
//! `gpui_wgpu`'s cosmic-text system over the fonts in /system/fonts.

#[cfg(target_os = "android")]
pub mod android;

#[cfg(target_os = "android")]
pub use android::{AndroidPlatform, activity, assets, current_platform, files_dir, start, vm};
/// Every window's view, the activity's own (id 0) and each after it, and the activity's lifecycle.
#[cfg(target_os = "android")]
pub use android::others as windows;
