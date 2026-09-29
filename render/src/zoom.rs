//! Magnifies the centre of the game's frame.
//!
//! Blitting a region of the default framebuffer onto itself with overlapping rectangles is
//! undefined in GL, so this copies the centre into its own texture at the same size (which
//! also resolves a driver-forced MSAA back buffer) and then stretches that over the screen.

use glow::HasContext as _;

pub struct Zoom {
    target: Option<Target>,
    supported: bool,
    reported: bool,
}

struct Target {
    framebuffer: glow::Framebuffer,
    texture: glow::Texture,
    size: [i32; 2],
}

impl Zoom {
    pub fn new(gl: &glow::Context) -> Self {
        // glBlitFramebuffer needs GL 3.0.
        let supported = gl.version().major >= 3;
        Self {
            target: None,
            supported,
            reported: false,
        }
    }

    /// Replaces the frame with its centre enlarged `factor` times.
    ///
    /// # Safety
    /// The overlay's GL context must be current on the game's drawable.
    pub unsafe fn draw(&mut self, gl: &glow::Context, screen: [u32; 2], factor: f32, smooth: bool) {
        if !self.supported {
            if !self.reported {
                log::warn!("zoom needs OpenGL 3.0 or later");
                self.reported = true;
            }
            return;
        }
        let (w, h) = (screen[0] as i32, screen[1] as i32);
        // Also rejects NaN.
        if w <= 0 || h <= 0 || factor.partial_cmp(&1.0) != Some(std::cmp::Ordering::Greater) {
            return;
        }
        let rw = ((w as f32 / factor).round() as i32).clamp(1, w);
        let rh = ((h as f32 / factor).round() as i32).clamp(1, h);
        let (x0, y0) = ((w - rw) / 2, (h - rh) / 2);
        // SAFETY: guaranteed by the caller.
        unsafe {
            let Some(framebuffer) = self.ensure_target(gl, [rw, rh]) else {
                return;
            };
            // Blits are clipped by the scissor box, which egui leaves enabled.
            gl.disable(glow::SCISSOR_TEST);
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, None);
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(framebuffer));
            gl.blit_framebuffer(
                x0,
                y0,
                x0 + rw,
                y0 + rh,
                0,
                0,
                rw,
                rh,
                glow::COLOR_BUFFER_BIT,
                glow::NEAREST,
            );
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(framebuffer));
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, None);
            gl.blit_framebuffer(
                0,
                0,
                rw,
                rh,
                0,
                0,
                w,
                h,
                glow::COLOR_BUFFER_BIT,
                if smooth { glow::LINEAR } else { glow::NEAREST },
            );
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        }
    }

    /// # Safety
    /// As for [`Zoom::draw`].
    unsafe fn ensure_target(
        &mut self,
        gl: &glow::Context,
        size: [i32; 2],
    ) -> Option<glow::Framebuffer> {
        if let Some(target) = &self.target
            && target.size == size
        {
            return Some(target.framebuffer);
        }
        // SAFETY: guaranteed by the caller.
        unsafe {
            self.destroy(gl);
            let texture = gl.create_texture().ok()?;
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA8 as i32,
                size[0],
                size[1],
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(None),
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::LINEAR as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::LINEAR as i32,
            );
            gl.bind_texture(glow::TEXTURE_2D, None);
            let framebuffer = match gl.create_framebuffer() {
                Ok(fb) => fb,
                Err(_) => {
                    gl.delete_texture(texture);
                    return None;
                }
            };
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(texture),
                0,
            );
            let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            self.target = Some(Target {
                framebuffer,
                texture,
                size,
            });
            if status != glow::FRAMEBUFFER_COMPLETE {
                log::warn!("zoom framebuffer incomplete (0x{status:X}); zoom disabled");
                self.destroy(gl);
                self.supported = false;
                return None;
            }
            Some(framebuffer)
        }
    }

    /// Frees the GL objects.
    ///
    /// # Safety
    /// As for [`Zoom::draw`].
    pub unsafe fn destroy(&mut self, gl: &glow::Context) {
        if let Some(target) = self.target.take() {
            // SAFETY: guaranteed by the caller.
            unsafe {
                gl.delete_framebuffer(target.framebuffer);
                gl.delete_texture(target.texture);
            }
        }
    }
}
