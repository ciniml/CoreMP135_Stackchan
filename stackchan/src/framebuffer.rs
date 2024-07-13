use std::convert::Infallible;

use embedded_graphics::{draw_target::DrawTarget, geometry::Dimensions, pixelcolor::{raw::RawU16, Rgb565, RgbColor}, prelude::RawData};
use framebuffer::Framebuffer;

pub struct FbdevDisplay {
    framebuffer: Vec<u8>,
    dev: Framebuffer,
}

impl FbdevDisplay {
    pub fn new(fbdev_path: &str) -> Self {
        let dev = Framebuffer::new(fbdev_path).unwrap();
        let framebuffer = vec![0; dev.var_screen_info.xres as usize * dev.var_screen_info.yres as usize * 2];
        Self { framebuffer, dev }
    }

    pub fn update(&mut self) {
        self.dev.write_frame(&self.framebuffer);
    }
}

impl DrawTarget for FbdevDisplay {
    type Color = Rgb565;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = embedded_graphics::Pixel<Self::Color>> {
        let screen_width = self.dev.var_screen_info.xres;
        let screen_height = self.dev.var_screen_info.yres;

        for pixel in pixels.into_iter() {
            let x = pixel.0.x as u32;
            let y = pixel.0.y as u32;
            // rotation
            let x = screen_width - x - 1;
            let y = screen_height - y - 1;

            let color = pixel.1;
            let raw_color: u16 = RawU16::from(color).into_inner();
            if x < self.dev.var_screen_info.xres && y < self.dev.var_screen_info.yres {
                let index = (x + y * self.dev.var_screen_info.xres) as usize * 2;
                self.framebuffer[index] = (raw_color & 0xff) as u8;
                self.framebuffer[index + 1] = (raw_color >> 8) as u8;
            }
        }

        Ok(())
    }

    // fn fill_contiguous<I>(&mut self, area: &embedded_graphics::primitives::Rectangle, colors: I) -> Result<(), Self::Error>
    //     where
    //         I: IntoIterator<Item = Self::Color>, {
    //     let mut y = area.top_left.y as usize;
    //     for color in colors.into_iter() {
    //         if y >= area.top_left.y as usize + area.size.height as usize {
    //             break;
    //         }
    //         let mut start_byte_index = y as usize * self.dev.var_screen_info.xres as usize * 2 + area.top_left.x as usize * 2;
    //         let raw_color: u16 = RawU16::from(color).into_inner();
    //         for _ in 0..area.size.width {
    //             self.framebuffer[start_byte_index] = (raw_color & 0xff) as u8;
    //             self.framebuffer[start_byte_index + 1] = (raw_color >> 8) as u8;
    //             start_byte_index += 2;
    //         }
    //         y += 1;
    //     }

    //     Ok(())
    // }
    
}

impl Dimensions for FbdevDisplay {
    fn bounding_box(&self) -> embedded_graphics::primitives::Rectangle {
        embedded_graphics::primitives::Rectangle::new(
            embedded_graphics::geometry::Point::new(0, 0),
            embedded_graphics::geometry::Size::new(
                self.dev.var_screen_info.xres,
                self.dev.var_screen_info.yres,
            ),
        )
    }
}