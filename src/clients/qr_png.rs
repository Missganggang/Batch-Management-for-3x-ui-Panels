use anyhow::{Context, Result};
use qrcode::{Color, QrCode};

pub(super) fn node_qr(link: &str) -> Result<Vec<u8>> {
    let code = QrCode::new(link.as_bytes()).context("链接过长，无法生成二维码")?;
    let modules = code.width();
    // Four blank modules form the standard QR quiet zone. Render at integer
    // scale so exporting or extracting the original PNG keeps sharp edges.
    const BORDER: usize = 4;
    const SCALE: usize = 6;
    let side = (modules + BORDER * 2) * SCALE;
    let stride = side.div_ceil(8);
    let mut pixels = vec![0xffu8; stride * side];
    for y in 0..modules {
        let start = (y + BORDER) * SCALE * stride;
        for x in 0..modules {
            if code[(x, y)] == Color::Dark {
                for px in (x + BORDER) * SCALE..(x + BORDER + 1) * SCALE {
                    pixels[start + px / 8] &= !(0x80 >> (px % 8));
                }
            }
        }
        for dy in 1..SCALE {
            pixels.copy_within(start..start + stride, start + dy * stride);
        }
    }
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, side as u32, side as u32);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::One);
        let mut writer = encoder.write_header().context("无法生成 PNG 图像头")?;
        writer
            .write_image_data(&pixels)
            .context("无法写入二维码图像")?;
        writer.finish().context("无法完成二维码图像")?;
    }
    Ok(bytes)
}
