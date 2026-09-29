//! Magnifies the centre of the game's frame.
//!
//! Blitting a region of the default framebuffer onto itself with overlapping rectangles is
//! undefined in GL, so this copies the centre into its own texture at the same size (which
//! also resolves a driver-forced MSAA back buffer) and then stretches that over the screen.

use glow::HasContext as _;

/// For the high-resolution zoom: the size the game should render at so that the middle
/// `real`-sized part of its frame shows the view enlarged `factor` times, and the factor
/// actually reached. Minecraft's field of view is vertical, so a frame `factor` times
/// taller at the same width packs `factor` times more pixels into every degree, in both
/// directions. `max_dim` is the largest texture the GPU takes.
pub fn tall_size(real: [u32; 2], factor: f32, max_dim: u32) -> Option<([u32; 2], f32)> {
    let [w, h] = real;
    if w == 0
        || h == 0
        || w > max_dim
        || factor.partial_cmp(&1.0) != Some(std::cmp::Ordering::Greater)
    {
        return None;
    }
    let tall = (f64::from(h) * f64::from(factor))
        .round()
        .min(f64::from(max_dim)) as u32;
    (tall > h).then(|| ([w, tall], tall as f32 / h as f32))
}

/// First row of the middle `real_h` rows of a `tall_h`-row frame.
pub fn middle_row(tall_h: u32, real_h: u32) -> u32 {
    tall_h.saturating_sub(real_h) / 2
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tall_size_multiplies_the_height() {
        assert_eq!(
            tall_size([2560, 1440], 4.0, 16384),
            Some(([2560, 5760], 4.0))
        );
        assert_eq!(middle_row(5760, 1440), 2160);
    }

    #[test]
    fn tall_size_is_capped_by_the_gpu() {
        let ([w, h], factor) = tall_size([2560, 1440], 16.0, 16384).unwrap();
        assert_eq!([w, h], [2560, 16384]);
        assert!((factor - 16384.0 / 1440.0).abs() < 1e-4);
    }

    #[test]
    fn nothing_to_gain() {
        assert_eq!(tall_size([2560, 1440], 1.0, 16384), None);
        assert_eq!(tall_size([2560, 1440], f32::NAN, 16384), None);
        assert_eq!(tall_size([2560, 16384], 2.0, 16384), None);
        assert_eq!(tall_size([0, 1440], 2.0, 16384), None);
        assert_eq!(tall_size([20000, 100], 2.0, 16384), None);
    }
}
