//! A window's renderer: `gpui_wgpu`'s on Metal by default, as `gpui_macos` draws, or `gpui_apple`'s
//! `MetalRenderer` under the `metal-renderer` feature. Both draw into the window view's own
//! `CAMetalLayer` (its `layerClass`), which outlives the renderer: the window holds the view.
//!
//! The window calls only what both answer: `update_drawable_size`, `draw` and `sprite_atlas`.

use anyhow::Result;
use objc2_quartz_core::CAMetalLayer;

#[cfg(not(feature = "metal-renderer"))]
mod imp {
    use super::*;
    use gpui::{DevicePixels, size};
    use gpui_wgpu::{GpuContext, WgpuRenderer, WgpuSurfaceConfig};

    /// One GPU device, shared by every window.
    pub type Context = GpuContext;
    pub type Renderer = WgpuRenderer;

    pub fn new_renderer(context: Context, layer: &CAMetalLayer) -> Result<Renderer> {
        let config = WgpuSurfaceConfig {
            // The window sets the real size straight after.
            size: size(DevicePixels(1), DevicePixels(1)),
            transparent: false,
            preferred_present_mode: None,
        };
        // Safety: the layer is the window view's backing `CAMetalLayer`, and the window drops
        // its renderer before its view.
        let layer = (layer as *const CAMetalLayer).cast_mut().cast();
        unsafe { WgpuRenderer::new_from_layer(context, layer, config) }
    }
}

#[cfg(feature = "metal-renderer")]
mod imp {
    use super::*;
    use gpui_apple::metal_renderer::{Context as MetalContext, MetalRenderer};

    pub type Context = MetalContext;
    pub type Renderer = MetalRenderer;

    pub fn new_renderer(context: Context, layer: &CAMetalLayer) -> Result<Renderer> {
        MetalRenderer::from_layer(context, layer, false)
    }
}

pub(super) use imp::{Context, Renderer, new_renderer};
