//! The backdrop blur (`BackdropBlur`): at one, the renderer ends its pass; this copies the region
//! under the box (grown by twice the radius) out of the target, blurs it by dual Kawase, two passes
//! down to a quarter and one up to a half, and draws the last pass up back into the target inside
//! the box's corners and clip. The renderer then begins a new pass. Its textures are kept from frame
//! to frame, at the target's size.

use anyhow::{Context as _, Result};
use gpui::BackdropBlur;
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_foundation::NSString;
use objc2_metal::{
    MTLBlendFactor, MTLBlendOperation, MTLBlitCommandEncoder, MTLClearColor, MTLCommandBuffer,
    MTLCommandEncoder, MTLDevice, MTLLibrary, MTLLoadAction, MTLOrigin, MTLPixelFormat,
    MTLPrimitiveType, MTLRenderCommandEncoder, MTLRenderPassDescriptor,
    MTLRenderPipelineDescriptor, MTLRenderPipelineState, MTLScissorRect, MTLSize, MTLStorageMode,
    MTLStoreAction, MTLTexture, MTLTextureDescriptor, MTLTextureUsage, MTLViewport,
};
use std::{mem, ptr};

/// `BlurParams` in shaders.metal.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct BlurParams {
    region: [f32; 4],
    bounds: [f32; 4],
    mask: [f32; 4],
    radii: [f32; 4],
    src_div: f32,
    dst_div: f32,
    offset: f32,
    pad: f32,
}

type Texture = Retained<ProtocolObject<dyn MTLTexture>>;
type Pipeline = Retained<ProtocolObject<dyn MTLRenderPipelineState>>;

pub(crate) struct Backdrop {
    down: Pipeline,
    up: Pipeline,
    back: Pipeline,
    /// Full size, half, quarter, half again; at `size`.
    textures: Option<[Texture; 4]>,
    size: (usize, usize),
}

impl Backdrop {
    pub(crate) fn new(
        device: &ProtocolObject<dyn MTLDevice>,
        library: &ProtocolObject<dyn MTLLibrary>,
        format: MTLPixelFormat,
    ) -> Result<Self> {
        Ok(Self {
            down: pipeline(device, library, "blur_down", "blur_down_fragment", format, false)?,
            up: pipeline(device, library, "blur_up", "blur_up_fragment", format, false)?,
            back: pipeline(device, library, "blur_back", "blur_back_fragment", format, true)?,
            textures: None,
            size: (0, 0),
        })
    }

    fn textures(
        &mut self,
        device: &ProtocolObject<dyn MTLDevice>,
        format: MTLPixelFormat,
        width: usize,
        height: usize,
    ) -> Result<[Texture; 4]> {
        if self.size != (width, height) || self.textures.is_none() {
            let make = |div: usize| -> Result<Texture> {
                let descriptor = MTLTextureDescriptor::new();
                // SAFETY: The dimensions are at least 1 and this describes a 2D texture.
                unsafe {
                    descriptor.setWidth(width.div_ceil(div).max(1));
                    descriptor.setHeight(height.div_ceil(div).max(1));
                }
                descriptor.setPixelFormat(format);
                descriptor.setStorageMode(MTLStorageMode::Private);
                descriptor.setUsage(MTLTextureUsage::RenderTarget | MTLTextureUsage::ShaderRead);
                device
                    .newTextureWithDescriptor(&descriptor)
                    .context("allocating a backdrop blur texture")
            };
            self.textures = Some([make(1)?, make(2)?, make(4)?, make(2)?]);
            self.size = (width, height);
        }
        self.textures.clone().context("backdrop blur textures")
    }

    /// Blurs `blur` in `target`, which must allow blits (a layer's drawable, not framebuffer-only).
    /// No render pass of `command_buffer` may be open.
    pub(crate) fn run(
        &mut self,
        device: &ProtocolObject<dyn MTLDevice>,
        blur: &BackdropBlur,
        target: &ProtocolObject<dyn MTLTexture>,
        command_buffer: &ProtocolObject<dyn MTLCommandBuffer>,
    ) -> Result<()> {
        let (tw, th) = (target.width() as f32, target.height() as f32);
        let b = blur.bounds;
        let (bx, by, bw, bh) = (b.origin.x.0, b.origin.y.0, b.size.width.0, b.size.height.0);
        let m = blur.content_mask.bounds;
        let (mx, my, mw, mh) = (m.origin.x.0, m.origin.y.0, m.size.width.0, m.size.height.0);
        let radius = blur.radius.0;
        // The region: the box grown by twice the radius, within the target.
        let grow = 2.0 * radius;
        let x0 = (bx - grow).floor().max(0.0);
        let y0 = (by - grow).floor().max(0.0);
        let x1 = (bx + bw + grow).ceil().min(tw);
        let y1 = (by + bh + grow).ceil().min(th);
        // Its part that is drawn back: the box within its clip and the target.
        let sx0 = bx.max(mx).floor().max(x0);
        let sy0 = by.max(my).floor().max(y0);
        let sx1 = (bx + bw).min(mx + mw).ceil().min(x1);
        let sy1 = (by + bh).min(my + mh).ceil().min(y1);
        if radius <= 0.0 || x1 <= x0 || y1 <= y0 || sx1 <= sx0 || sy1 <= sy0 {
            return Ok(());
        }
        let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
        let format = target.pixelFormat();
        let [full, half, quarter, up] = self.textures(
            device,
            format,
            target.width().max(1),
            target.height().max(1),
        )?;
        let r = blur.corner_radii;
        let mut params = BlurParams {
            region: [x0, y0, w as f32, h as f32],
            bounds: [bx, by, bw, bh],
            mask: [mx, my, mw, mh],
            radii: [r.top_left.0, r.top_right.0, r.bottom_right.0, r.bottom_left.0],
            // Dual Kawase's offset for this radius: a CSS blur's radius is its sigma.
            offset: (radius / 6.0).max(0.5),
            ..BlurParams::default()
        };

        let blit = command_buffer
            .blitCommandEncoder()
            .context("creating the backdrop blur's blit encoder")?;
        // SAFETY: The region lies within `target`, and `full` is at least the target's size.
        unsafe {
            blit.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toTexture_destinationSlice_destinationLevel_destinationOrigin(
                target,
                0,
                0,
                MTLOrigin { x: x0 as usize, y: y0 as usize, z: 0 },
                MTLSize { width: w, height: h, depth: 1 },
                &full,
                0,
                0,
                MTLOrigin { x: 0, y: 0, z: 0 },
            );
        }
        blit.endEncoding();

        // down: full to half, half to quarter; up: quarter to half; back: half to the target.
        let passes: [(&Pipeline, &Texture, &Texture, f32, f32); 3] = [
            (&self.down, &full, &half, 1.0, 2.0),
            (&self.down, &half, &quarter, 2.0, 4.0),
            (&self.up, &quarter, &up, 4.0, 2.0),
        ];
        for (pipeline, src, dst, src_div, dst_div) in passes {
            params.src_div = src_div;
            params.dst_div = dst_div;
            let encoder = encoder(command_buffer, dst, true)?;
            encoder.setViewport(MTLViewport {
                originX: 0.0,
                originY: 0.0,
                width: (w as f32 / dst_div).ceil() as f64,
                height: (h as f32 / dst_div).ceil() as f64,
                znear: 0.0,
                zfar: 1.0,
            });
            draw(&encoder, pipeline, src, &params);
            encoder.endEncoding();
        }

        params.src_div = 2.0;
        params.dst_div = 1.0;
        let encoder = encoder(command_buffer, target, false)?;
        encoder.setViewport(MTLViewport {
            originX: 0.0,
            originY: 0.0,
            width: tw as f64,
            height: th as f64,
            znear: 0.0,
            zfar: 1.0,
        });
        encoder.setScissorRect(MTLScissorRect {
            x: sx0 as usize,
            y: sy0 as usize,
            width: (sx1 - sx0) as usize,
            height: (sy1 - sy0) as usize,
        });
        draw(&encoder, &self.back, &up, &params);
        encoder.endEncoding();
        Ok(())
    }
}

fn encoder(
    command_buffer: &ProtocolObject<dyn MTLCommandBuffer>,
    texture: &ProtocolObject<dyn MTLTexture>,
    clear: bool,
) -> Result<Retained<ProtocolObject<dyn MTLRenderCommandEncoder>>> {
    let descriptor = MTLRenderPassDescriptor::new();
    // SAFETY: Index zero is within the render pass's color attachment array.
    let attachment = unsafe { descriptor.colorAttachments().objectAtIndexedSubscript(0) };
    attachment.setTexture(Some(texture));
    attachment.setStoreAction(MTLStoreAction::Store);
    if clear {
        attachment.setLoadAction(MTLLoadAction::Clear);
        attachment.setClearColor(MTLClearColor { red: 0., green: 0., blue: 0., alpha: 0. });
    } else {
        attachment.setLoadAction(MTLLoadAction::Load);
    }
    command_buffer
        .renderCommandEncoderWithDescriptor(&descriptor)
        .context("creating a backdrop blur pass")
}

fn draw(
    encoder: &ProtocolObject<dyn MTLRenderCommandEncoder>,
    pipeline: &ProtocolObject<dyn MTLRenderPipelineState>,
    src: &ProtocolObject<dyn MTLTexture>,
    params: &BlurParams,
) {
    encoder.setRenderPipelineState(pipeline);
    // SAFETY: Metal copies the parameters at their exact size; the shader reads `BlurParams` at
    // buffer 0 and its source at texture 0, and draws one triangle over the viewport.
    unsafe {
        encoder.setFragmentBytes_length_atIndex(
            ptr::NonNull::from(params).cast(),
            mem::size_of::<BlurParams>(),
            0,
        );
        encoder.setFragmentTexture_atIndex(Some(src), 0);
        encoder.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::Triangle, 0, 3);
    }
}

fn pipeline(
    device: &ProtocolObject<dyn MTLDevice>,
    library: &ProtocolObject<dyn MTLLibrary>,
    label: &str,
    fragment_fn_name: &str,
    format: MTLPixelFormat,
    premultiplied_over: bool,
) -> Result<Pipeline> {
    let vertex_fn = library
        .newFunctionWithName(&NSString::from_str("blur_vertex"))
        .context("locating vertex function blur_vertex")?;
    let fragment_fn = library
        .newFunctionWithName(&NSString::from_str(fragment_fn_name))
        .with_context(|| format!("locating fragment function {fragment_fn_name}"))?;
    let descriptor = MTLRenderPipelineDescriptor::new();
    descriptor.setLabel(Some(&NSString::from_str(label)));
    descriptor.setVertexFunction(Some(vertex_fn.as_ref()));
    descriptor.setFragmentFunction(Some(fragment_fn.as_ref()));
    // SAFETY: Index zero is within the pipeline's color attachment array.
    let attachment = unsafe { descriptor.colorAttachments().objectAtIndexedSubscript(0) };
    attachment.setPixelFormat(format);
    // Back into the target, premultiplied: what is under the box is replaced where it is covered.
    attachment.setBlendingEnabled(premultiplied_over);
    if premultiplied_over {
        attachment.setRgbBlendOperation(MTLBlendOperation::Add);
        attachment.setAlphaBlendOperation(MTLBlendOperation::Add);
        attachment.setSourceRGBBlendFactor(MTLBlendFactor::One);
        attachment.setSourceAlphaBlendFactor(MTLBlendFactor::One);
        attachment.setDestinationRGBBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
        attachment.setDestinationAlphaBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
    }
    device
        .newRenderPipelineStateWithDescriptor_error(&descriptor)
        .with_context(|| format!("creating {label} render pipeline"))
}

/// Patterns, dotted borders and the blur, drawn by the headless renderer.
#[cfg(test)]
mod tests {
    use crate::metal_renderer::{InstanceBufferPool, MetalRenderer};
    use gpui::{
        BackdropBlur, BorderStyle, Bounds, ContentMask, Corners, DevicePixels, Edges, Hsla,
        PatternLines, Point, Quad, ScaledPixels, Scene, Size, pattern_dots, pattern_lines,
    };
    use parking_lot::Mutex;
    use std::sync::Arc;

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Bounds<ScaledPixels> {
        Bounds {
            origin: Point { x: x.into(), y: y.into() },
            size: Size { width: w.into(), height: h.into() },
        }
    }

    fn quad(bounds: Bounds<ScaledPixels>, background: gpui::Background) -> Quad {
        Quad {
            order: 0,
            border_style: BorderStyle::Solid,
            bounds,
            content_mask: ContentMask { bounds, ..Default::default() },
            background,
            border_color: Hsla::transparent_black(),
            corner_radii: Corners::default(),
            border_widths: Edges::default(),
            pad: [0; 2],
        }
    }

    fn draw(scene: &mut Scene, w: i32, h: i32) -> image::RgbaImage {
        scene.finish();
        MetalRenderer::new_headless(Arc::new(Mutex::new(InstanceBufferPool::default())))
            .expect("a headless renderer")
            .render_scene_to_image(scene, Size { width: DevicePixels(w), height: DevicePixels(h) })
            .expect("a frame")
    }

    fn red(image: &image::RgbaImage, x: u32, y: u32) -> u8 {
        image.get_pixel(x, y).0[0]
    }

    #[test]
    fn dots_sit_at_each_tiles_centre() {
        let mut scene = Scene::default();
        scene.insert_primitive(quad(rect(0., 0., 32., 32.), pattern_dots(gpui::red(), 16., 2.)));
        let image = draw(&mut scene, 32, 32);
        assert!(red(&image, 8, 8) > 240 && red(&image, 24, 24) > 240, "a dot at each centre");
        assert!(red(&image, 0, 0) < 15 && red(&image, 15, 8) < 15, "nothing between them");
    }

    #[test]
    fn grid_lines_run_from_the_corner() {
        let mut scene = Scene::default();
        scene.insert_primitive(quad(rect(0., 0., 32., 32.), pattern_lines(gpui::red(), 16., 1., PatternLines::Grid)));
        scene.insert_primitive(quad(rect(0., 40., 32., 32.), pattern_lines(gpui::red(), 16., 1., PatternLines::Horizontal)));
        let image = draw(&mut scene, 32, 72);
        assert!(red(&image, 0, 5) > 240 && red(&image, 16, 5) > 240 && red(&image, 5, 16) > 240, "both ways");
        assert!(red(&image, 5, 5) < 15, "nothing inside a tile");
        assert!(red(&image, 5, 56) > 240 && red(&image, 0, 45) < 15, "horizontal only");
    }

    #[test]
    fn a_dotted_border_is_round_dots_with_gaps() {
        let mut scene = Scene::default();
        let mut q = quad(rect(0., 0., 64., 16.), Hsla::transparent_black().into());
        q.border_style = BorderStyle::Dotted;
        q.border_color = gpui::red();
        q.border_widths = Edges { top: 4.0.into(), ..Default::default() };
        scene.insert_primitive(q);
        let image = draw(&mut scene, 64, 16);
        let row = |y: u32| (0..64).map(|x| red(&image, x, y) as u32).collect::<Vec<_>>();
        let (edge, middle) = (row(0), row(2));
        assert!(middle.iter().any(|&r| r > 200) && middle.iter().any(|&r| r < 15), "dots and gaps: {middle:?}");
        assert!(edge.iter().sum::<u32>() < middle.iter().sum::<u32>(), "round: thinner at the edge than the middle");
    }

    #[test]
    fn a_backdrop_blur_softens_only_inside_its_box() {
        let mut scene = Scene::default();
        // White on the left half, clear on the right; a box over the edge blurs it.
        scene.insert_primitive(quad(rect(0., 0., 32., 64.), gpui::white().into()));
        scene.insert_primitive(BackdropBlur {
            order: 0,
            radius: 8.0.into(),
            bounds: rect(16., 16., 32., 32.),
            content_mask: ContentMask { bounds: rect(0., 0., 64., 64.), ..Default::default() },
            corner_radii: Corners::default(),
        });
        let image = draw(&mut scene, 64, 64);
        let (inside_white, inside_clear) = (red(&image, 30, 32), red(&image, 34, 32));
        assert!(inside_white < 245 && inside_clear > 10, "the edge is soft in the box: {inside_white} {inside_clear}");
        assert!(red(&image, 31, 8) > 250 && red(&image, 32, 8) < 5, "and sharp outside it");
        assert!(red(&image, 17, 32) > 245, "far from the edge the white stays");
    }
}
