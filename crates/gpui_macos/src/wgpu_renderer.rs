//! A window's renderer on `gpui_wgpu`, drawing on Metal: the names the window calls, over
//! `WgpuRenderer`. `gpui_apple`'s `MetalRenderer` takes its place under the `metal-renderer`
//! feature.
//!
//! The renderer owns the view's backing `CAMetalLayer`, as `MetalRenderer` does, and wgpu draws
//! into it. wgpu takes each frame's drawable from the layer itself, so the window paces frames
//! with its `CVDisplayLink` rather than a `CAMetalDisplayLink` that hands drawables out:
//! `layer()` answers `None`, and the window picks its display link path from that.

use anyhow::Result;
use gpui::{DevicePixels, Scene, Size, size};
use gpui_wgpu::{GpuContext, WgpuAtlas, WgpuRenderer, WgpuSurfaceConfig, wgpu};
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_quartz_core::{CAAutoresizingMask, CAMetalDrawable, CAMetalLayer};
use std::sync::Arc;

/// One GPU device, shared by every window.
pub type Context = GpuContext;
pub type Renderer = WgpuMacRenderer;

pub fn new_renderer(context: Context, transparent: bool, use_display_p3: bool) -> Result<Renderer> {
    WgpuMacRenderer::new(context, transparent, use_display_p3)
}

pub struct WgpuMacRenderer {
    renderer: WgpuRenderer,
    // Dropped after `renderer`, whose surface draws into it.
    layer: Retained<CAMetalLayer>,
}

impl WgpuMacRenderer {
    fn new(context: Context, transparent: bool, use_display_p3: bool) -> Result<Self> {
        let layer = CAMetalLayer::new();
        layer.setNeedsDisplayOnBoundsChange(true);
        layer.setAutoresizingMask(
            CAAutoresizingMask::LayerWidthSizable | CAAutoresizingMask::LayerHeightSizable,
        );
        let config = WgpuSurfaceConfig {
            // The window sets the real size once the view is laid out.
            size: size(DevicePixels(1), DevicePixels(1)),
            transparent,
            preferred_present_mode: None,
        };
        // Safety: the layer is a `CAMetalLayer`, and this struct drops the renderer before it.
        let mut renderer = unsafe {
            WgpuRenderer::new_from_layer(
                context,
                Retained::as_ptr(&layer).cast_mut().cast(),
                config,
            )?
        };
        if use_display_p3 {
            renderer.set_color_space(wgpu::SurfaceColorSpace::DisplayP3);
        }
        Ok(Self { renderer, layer })
    }

    /// `None`: wgpu takes its own drawables, so no `CAMetalDisplayLink` drives this layer.
    pub fn layer(&self) -> Option<&CAMetalLayer> {
        None
    }

    /// The view's backing layer.
    pub fn layer_ptr(&self) -> *mut CAMetalLayer {
        Retained::as_ptr(&self.layer).cast_mut()
    }

    pub fn sprite_atlas(&self) -> &Arc<WgpuAtlas> {
        self.renderer.sprite_atlas()
    }

    /// Never handed one, since `layer()` is `None`; a drawable given anyway goes back unused.
    pub fn set_frame_drawable(
        &mut self,
        drawable: Option<Retained<ProtocolObject<dyn CAMetalDrawable>>>,
    ) -> Option<Retained<ProtocolObject<dyn CAMetalDrawable>>> {
        drawable
    }

    pub fn set_display_link_attached(&mut self, _attached: bool) {}

    /// During a live resize the window draws synchronously, and the frame must reach the
    /// screen in the same Core Animation transaction as the new window size; wgpu reads this
    /// flag off the layer when it presents.
    pub fn set_presents_with_transaction(&mut self, presents_with_transaction: bool) {
        self.layer
            .setPresentsWithTransaction(presents_with_transaction);
    }

    pub fn update_drawable_size(&mut self, size: Size<DevicePixels>) {
        self.renderer.update_drawable_size(size);
    }

    pub fn update_transparency(&mut self, transparent: bool) {
        self.renderer.update_transparency(transparent);
    }

    pub fn destroy(&mut self) {
        self.renderer.destroy();
    }

    pub fn draw(&mut self, scene: &Scene) {
        self.renderer.draw(scene);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn render_to_image(&mut self, scene: &Scene) -> Result<image::RgbaImage> {
        self.renderer.render_to_image(scene)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Bounds, ContentMask, DevicePixels, Hsla, Quad, ScaledPixels, point, size};

    #[test]
    fn draws_a_scene_into_an_image_of_the_drawable_size() {
        let mut renderer = new_renderer(Default::default(), false, false).unwrap();
        assert!(renderer.layer().is_none(), "wgpu takes its own drawables");
        renderer.update_drawable_size(size(DevicePixels(64), DevicePixels(32)));
        let mut scene = Scene::default();
        let bounds = Bounds::new(
            point(ScaledPixels(0.), ScaledPixels(0.)),
            size(ScaledPixels(64.), ScaledPixels(32.)),
        );
        scene.insert_primitive(Quad {
            bounds,
            content_mask: ContentMask { bounds },
            background: Hsla::red().into(),
            border_color: Hsla::red(),
            ..Default::default()
        });
        scene.finish();
        let image = renderer.render_to_image(&scene).unwrap();
        assert_eq!((image.width(), image.height()), (64, 32));
        let pixel = image.get_pixel(32, 16);
        assert!(
            pixel[0] > 200 && pixel[1] < 50 && pixel[2] < 50,
            "a red quad fills the frame: {pixel:?}"
        );
    }
}
