//! Generate the public, synthetic QR recovery fixtures.
//!
//! These images contain only demo URLs owned by this project. Re-run with:
//! `cargo run -p qr-restore-wasm --example generate_samples`.

use image::{GrayImage, ImageResult, Luma, imageops};
use qrcode::{Color, EcLevel, QrCode};
use std::path::{Path, PathBuf};

const BLURRED_PAYLOAD: &[u8] = b"https://qrcode.toolbox.icu/demo/blurred";
const DEFORMED_PAYLOAD: &[u8] = b"https://qrcode.toolbox.icu/demo/deformed";
const QUIET_ZONE: usize = 4;

fn render_qr(payload: &[u8], scale: usize) -> GrayImage {
    let code = QrCode::with_error_correction_level(payload, EcLevel::M)
        .expect("synthetic payload must fit a QR code");
    let modules = code.width();
    let side = (modules + QUIET_ZONE * 2) * scale;
    let mut image = GrayImage::from_pixel(side as u32, side as u32, Luma([255]));

    for module_y in 0..modules {
        for module_x in 0..modules {
            if code[(module_x, module_y)] != Color::Dark {
                continue;
            }
            let start_x = (module_x + QUIET_ZONE) * scale;
            let start_y = (module_y + QUIET_ZONE) * scale;
            for pixel_y in start_y..start_y + scale {
                for pixel_x in start_x..start_x + scale {
                    image.put_pixel(pixel_x as u32, pixel_y as u32, Luma([0]));
                }
            }
        }
    }
    image
}

fn add_deterministic_camera_noise(image: &mut GrayImage) {
    let width = image.width().max(1);
    let height = image.height().max(1);
    for (x, y, pixel) in image.enumerate_pixels_mut() {
        let gradient = (x as f32 / width as f32 * 10.0 + y as f32 / height as f32 * 6.0) - 8.0;
        let hash = x.wrapping_mul(73_856_093) ^ y.wrapping_mul(19_349_663);
        let noise = (hash % 9) as f32 - 4.0;
        pixel.0[0] = (f32::from(pixel.0[0]) + gradient + noise).clamp(0.0, 255.0) as u8;
    }
}

fn synthetic_blurred() -> GrayImage {
    let clean = render_qr(BLURRED_PAYLOAD, 8);
    let blurred = imageops::blur(&clean, 1.6);
    let mut reduced = imageops::resize(
        &blurred,
        clean.width() / 2,
        clean.height() / 2,
        imageops::FilterType::Lanczos3,
    );
    add_deterministic_camera_noise(&mut reduced);
    reduced
}

fn bilinear_sample(image: &GrayImage, x: f32, y: f32) -> u8 {
    if x < 0.0 || y < 0.0 || x >= image.width() as f32 - 1.0 || y >= image.height() as f32 - 1.0 {
        return 255;
    }
    let x0 = x.floor() as u32;
    let y0 = y.floor() as u32;
    let x1 = x0 + 1;
    let y1 = y0 + 1;
    let tx = x - x0 as f32;
    let ty = y - y0 as f32;
    let top = f32::from(image.get_pixel(x0, y0).0[0]) * (1.0 - tx)
        + f32::from(image.get_pixel(x1, y0).0[0]) * tx;
    let bottom = f32::from(image.get_pixel(x0, y1).0[0]) * (1.0 - tx)
        + f32::from(image.get_pixel(x1, y1).0[0]) * tx;
    (top * (1.0 - ty) + bottom * ty).round() as u8
}

fn synthetic_deformed() -> GrayImage {
    let clean = render_qr(DEFORMED_PAYLOAD, 8);
    let width = clean.width();
    let height = clean.height();
    let center_x = (width - 1) as f32 / 2.0;
    let center_y = (height - 1) as f32 / 2.0;
    let mut warped = GrayImage::from_pixel(width, height, Luma([255]));

    for y in 0..height {
        for x in 0..width {
            let normalized_y = (y as f32 - center_y) / center_y;
            let normalized_x = (x as f32 - center_x) / center_x;
            let row_scale = 1.0 + 0.025 * (1.0 - normalized_y * normalized_y);
            let bend = 2.0 * (normalized_y * std::f32::consts::FRAC_PI_2).sin();
            let source_x = center_x + (x as f32 - center_x - bend) / row_scale;
            let source_y = center_y + (y as f32 - center_y) - normalized_x;
            warped.put_pixel(x, y, Luma([bilinear_sample(&clean, source_x, source_y)]));
        }
    }

    let mut softened = imageops::blur(&warped, 0.4);
    add_deterministic_camera_noise(&mut softened);
    softened
}

fn output_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples")
}

fn main() -> ImageResult<()> {
    let output = output_dir();
    std::fs::create_dir_all(&output)?;
    synthetic_blurred().save(output.join("synthetic-blurred.png"))?;
    synthetic_deformed().save(output.join("synthetic-deformed.png"))?;
    println!(
        "Generated public synthetic QR fixtures in {}",
        output.display()
    );
    Ok(())
}
