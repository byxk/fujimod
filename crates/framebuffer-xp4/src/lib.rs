//! XP4 LCDC framebuffer decoding without timing or device effects.
use fujimod_framebuffer::{
    Layer, Output,
    raster::{PixelOrder, Plane},
};
/// Selected by the processor graph; no implicit pixel-format fallback.
#[derive(Clone, Copy)]
pub enum Picture {
    /// BGRA with straight alpha, except that a pixel whose color bytes are all zero is not
    /// drawn whatever its alpha byte holds: observed on an X-T3 5.11, opaque black (alpha
    /// 0xff) and 50 % black (alpha 0x80) picture pixels left the live view below them
    /// unchanged, while opaque white pixels covered it.
    Bgra8888Straight,
}
/// Explicit linear video transport; hardware packing is not inferred from RAM.
#[derive(Clone, Copy)]
pub enum Video {
    YyCbCr422,
}
pub const BASES: [u64; 2] = [0xfff7a000, 0xfffa9000];
pub const REGISTER_BYTES: usize = 0x1a0;
struct Decoder<'a> {
    regs: &'a [u32],
    picture: Option<Picture>,
    video: Option<Video>,
}
impl Decoder<'_> {
    fn r(&self, o: usize) -> u32 {
        self.regs[o / 4]
    }
    fn pair(&self, offset: usize, mask: u32) -> Result<(u32, u32), String> {
        let word = self.r(offset);
        if word & !(mask | mask << 16) != 0 {
            return Err("Unsupported XP4 LCDC geometry bits".into());
        }
        Ok((word & mask, word >> 16))
    }
    fn plane(&self, video: bool, width: u32, height: u32) -> Result<Plane, String> {
        let (address, stride, geometry, order) = if video {
            if self.video.is_none() || self.r(0x164) > 3 || self.r(0x16c) & !0xfff != 0 {
                return Err("Unsupported XP4 LCDC video transport/address/packing".into());
            }
            // 010ed130 writes descriptor+1ea (pitch in pixels) /4 to +16c.
            // This selected packed422 transport therefore consumes 8 bytes
            // per pitch unit, unlike the raw BGRA plane's pixel pitch at+50.
            (
                ((u64::from(self.r(0x164))) << 32) | u64::from(self.r(0x168)),
                (self.r(0x16c) & 0xfff) * 8,
                0x170,
                PixelOrder::YyCbCr,
            )
        } else {
            let source = (u64::from(self.r(0x40)) << 32) | u64::from(self.r(0x44));
            let alternate = (u64::from(self.r(0x48)) << 32) | u64::from(self.r(0x4c));
            if self.picture.is_none() || self.r(0x40) > 3 || source != alternate {
                return Err("Unsupported XP4 LCDC alternate picture selection".into());
            }
            if self.r(0x50) & !0xfff != 0x10000 {
                return Err("Unsupported XP4 LCDC raw picture mode".into());
            }
            (source, (self.r(0x50) & 0xfff) * 4, 0x54, PixelOrder::BgraBlackKeyed)
        };
        if self.r(geometry + 0x10) != 0 || self.r(geometry + 0x14) != 0 {
            return Err("Unsupported XP4 LCDC picture crop or fractional phase".into());
        }
        let (sw, sh) = self.pair(geometry, 0xfff)?;
        let (x, y) = self.pair(geometry + 4, 0x1fff)?;
        let (w, h) = self.pair(geometry + 8, 0xfff)?;
        let (dx, dy) = self.pair(geometry + 12, 0x7fff)?;
        if sw == 0
            || sh == 0
            || w == 0
            || h == 0
            || dx == 0
            || dy == 0
            || address == 0
            || address & 3 != 0
            || x + w > width
            || y + h > height
            || (((w - 1) * dx) >> 12) >= sw
            || (((h - 1) * dy) >> 12) >= sh
        {
            return Err("Invalid XP4 LCDC picture geometry/address".into());
        }
        Ok(Plane {
            address,
            stride,
            source_x: 0,
            source_y: 0,
            source_width: sw,
            source_height: sh,
            sampling: Some([dx, dy]),
            work_x: x,
            work_y: y,
            work_width: w,
            work_height: h,
            order,
        })
    }
}
pub fn decode(
    regs: &[u32],
    picture: Option<Picture>,
    video: Option<Video>,
) -> Result<Option<Output>, String> {
    if regs.len() < REGISTER_BYTES / 4 {
        return Err("XP4 display register snapshot is truncated".into());
    }
    if regs[1] != 1 {
        return Ok(None);
    }
    let d = Decoder {
        regs,
        picture,
        video,
    };
    let enable = d.r(8);
    if enable & !0x11010 != 0 {
        return Err("Unsupported XP4 LCDC active layer selection".into());
    }
    let width = d.r(0x14) & 0x3fff;
    let height = d.r(0x20) & 0x1fff;
    fujimod_framebuffer::pixel_bytes(width, height)?;
    let mut layers = vec![];
    for (bit, video) in [(4, true), (12, false)] {
        if enable & (1 << bit) != 0 {
            layers.push(Layer {
                id: bit.to_string(),
                plane: d.plane(video, width, height)?,
            });
        }
    }
    Ok((!layers.is_empty()).then_some(Output {
        width,
        height,
        layers,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn raw_plane_preserves_stride_scaling_and_alpha() {
        let mut regs = vec![0; REGISTER_BYTES / 4];
        for (o, v) in [
            (4, 1),
            (8, 0x1000),
            (0x14, 4),
            (0x20, 2),
            (0x44, 16),
            (0x4c, 16),
            (0x50, 0x10002),
            (0x54, 0x10002),
            (0x5c, 0x20004),
            (0x60, 0x8000800),
        ] {
            regs[o / 4] = v;
        }
        let out = decode(&regs, Some(Picture::Bgra8888Straight), None)
            .unwrap()
            .unwrap();
        assert_eq!(out.layers[0].id, "12");
        let rgba = out
            .render(|a, n| {
                assert_eq!((a, n), (16, 8));
                Ok(vec![30, 20, 10, 255, 50, 40, 30, 255])
            })
            .unwrap();
        assert_eq!(&rgba[..8], &[10, 20, 30, 255, 10, 20, 30, 255]);
        regs[8 / 4] |= 2;
        assert!(decode(&regs, Some(Picture::Bgra8888Straight), None).is_err());
    }
    /// Observed on an X-T3 5.11, a picture over the REC live view: pixels written opaque
    /// black (0xff000000) and 50 % black (0x80000000) left the live view unchanged, while
    /// opaque white pixels covered it. A straight-alpha law drew black bands the camera never
    /// showed.
    #[test]
    fn black_picture_pixels_leave_the_video_below_them_visible() {
        let mut regs = vec![0; 0x604 / 4];
        for (o, v) in [
            (4, 1),
            (8, 0x11010),
            (0x14, 4),
            (0x20, 2),
            (0x44, 16),
            (0x4c, 16),
            (0x50, 0x10004),
            (0x54, 0x20004),
            (0x5c, 0x20004),
            (0x60, 0x10001000),
            (0x168, 64),
            (0x16c, 2),
            (0x170, 0x20004),
            (0x178, 0x20004),
            (0x17c, 0x10001000),
        ] {
            regs[o / 4] = v;
        }
        let picture: [[u8; 4]; 4] = [
            [0, 0, 0, 255],
            [0, 0, 0, 128],
            [255, 255, 255, 255],
            [8, 8, 8, 255],
        ];
        let mut ram = vec![0u8; 128];
        for row in 0..2 {
            for (n, pixel) in picture.iter().enumerate() {
                ram[16 + row * 16 + n * 4..][..4].copy_from_slice(pixel);
            }
        }
        for row in 0..2 {
            ram[64 + row * 16..][..8]
                .copy_from_slice(&[126, 126, 128, 128, 126, 126, 128, 128]);
        }
        let read = |a: u64, n: usize| Ok(ram[a as usize..a as usize + n].to_vec());
        let out = decode(&regs, Some(Picture::Bgra8888Straight), Some(Video::YyCbCr422))
            .unwrap()
            .unwrap();
        let rgba = out.render(read).unwrap();
        regs[8 / 4] = 0x10010;
        let video = decode(&regs, Some(Picture::Bgra8888Straight), Some(Video::YyCbCr422))
            .unwrap()
            .unwrap()
            .render(read)
            .unwrap();
        let gray = &video[..4];
        assert_ne!(gray, [0, 0, 0, 255]);
        for row in 0..2 {
            let pixels = &rgba[row * 16..][..16];
            assert_eq!(&pixels[..4], gray, "opaque black is not drawn");
            assert_eq!(&pixels[4..8], gray, "50 % black is not drawn");
            assert_eq!(&pixels[8..12], [255, 255, 255, 255]);
            assert_eq!(&pixels[12..16], [8, 8, 8, 255]);
        }
    }
}
