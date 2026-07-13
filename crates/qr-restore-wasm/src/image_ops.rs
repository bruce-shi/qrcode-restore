//! Image-processing primitives kept inside the QR Restore WebAssembly module.
//!
//! Photon supplies generic, audited RGBA transforms. QR-specific operations
//! remain here where their exact sampling and threshold semantics are part of
//! the recovery model.

use photon_rs::{
    PhotonImage, channels, monochrome,
    transform::{self, SamplingFilter},
};
use wasm_bindgen::prelude::*;

const MAX_INPUT_PIXELS: usize = 24_000_000;
const MAX_SIDE: u32 = 4096;

fn image_error(message: impl Into<String>) -> JsValue {
    JsValue::from_str(&message.into())
}

fn validate_dimensions(width: u32, height: u32, byte_len: usize) -> Result<(), String> {
    if width < 1 || height < 1 || width > MAX_SIDE || height > MAX_SIDE {
        return Err(format!(
            "image dimensions must be between 1 and {MAX_SIDE} pixels"
        ));
    }
    let pixels = width as usize * height as usize;
    if pixels > MAX_INPUT_PIXELS {
        return Err(format!("image exceeds the {MAX_INPUT_PIXELS} pixel limit"));
    }
    if byte_len != pixels * 4 {
        return Err(format!(
            "expected {} RGBA bytes for {width}x{height}, received {byte_len}",
            pixels * 4
        ));
    }
    Ok(())
}

fn target_dimensions(width: u32, height: u32, scale: f64) -> Result<(u32, u32), String> {
    if !scale.is_finite() || scale <= 0.0 {
        return Err("scale must be a positive finite number".into());
    }
    let target_width = (f64::from(width) * scale).round() as u32;
    let target_height = (f64::from(height) * scale).round() as u32;
    if target_width < 21 || target_height < 21 {
        return Err("resized image must be at least 21x21".into());
    }
    validate_dimensions(
        target_width,
        target_height,
        target_width as usize * target_height as usize * 4,
    )?;
    Ok((target_width, target_height))
}

fn clamp_u8(value: f64) -> u8 {
    if !value.is_finite() || value <= 0.0 {
        0
    } else if value >= 255.0 {
        255
    } else {
        // Uint8ClampedArray uses ties-to-even. Exact ties are rare in these
        // kernels, but preserving the rule keeps native tests deterministic.
        let floor = value.floor();
        let fraction = value - floor;
        if fraction > 0.5 || (fraction == 0.5 && floor as u64 % 2 == 1) {
            floor as u8 + 1
        } else {
            floor as u8
        }
    }
}

fn gray_pixels(values: &[u8]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(values.len() * 4);
    for &value in values {
        rgba.extend_from_slice(&[value, value, value, 255]);
    }
    rgba
}

fn intensity(pixels: &[u8], pixel: usize) -> u8 {
    pixels[pixel * 4]
}

fn lanczos(value: f64, radius: usize) -> f64 {
    let absolute = value.abs();
    if absolute < 1e-7 {
        return 1.0;
    }
    if absolute >= radius as f64 {
        return 0.0;
    }
    let pi_value = std::f64::consts::PI * absolute;
    (pi_value.sin() / pi_value) * ((pi_value / radius as f64).sin() / (pi_value / radius as f64))
}

#[wasm_bindgen]
pub struct WasmImage {
    inner: PhotonImage,
}

impl WasmImage {
    pub(crate) fn from_pixels(pixels: Vec<u8>, width: u32, height: u32) -> Self {
        Self {
            inner: PhotonImage::new(pixels, width, height),
        }
    }

    pub(crate) fn raw(&self) -> Vec<u8> {
        self.inner.get_raw_pixels()
    }
}

#[wasm_bindgen]
impl WasmImage {
    #[wasm_bindgen(constructor)]
    pub fn new(pixels: &[u8], width: u32, height: u32) -> Result<WasmImage, JsValue> {
        validate_dimensions(width, height, pixels.len()).map_err(image_error)?;
        Ok(Self::from_pixels(pixels.to_vec(), width, height))
    }

    #[wasm_bindgen(getter)]
    pub fn width(&self) -> u32 {
        self.inner.get_width()
    }

    #[wasm_bindgen(getter)]
    pub fn height(&self) -> u32 {
        self.inner.get_height()
    }

    pub fn pixels(&self) -> Vec<u8> {
        self.raw()
    }

    pub fn grayscale(&self, channel: &str) -> Result<WasmImage, JsValue> {
        let source = self.raw();
        let mut values = Vec::with_capacity(source.len() / 4);
        for rgba in source.chunks_exact(4) {
            let value = match channel {
                "red" => rgba[0],
                "green" => rgba[1],
                "blue" => rgba[2],
                "min" => rgba[0].min(rgba[1]).min(rgba[2]),
                "luma" => clamp_u8(
                    f64::from(rgba[0]) * 0.299
                        + f64::from(rgba[1]) * 0.587
                        + f64::from(rgba[2]) * 0.114,
                ),
                _ => {
                    return Err(image_error(
                        "channel must be luma, red, green, blue, or min",
                    ));
                }
            };
            values.push(value);
        }
        Ok(Self::from_pixels(
            gray_pixels(&values),
            self.width(),
            self.height(),
        ))
    }

    /// Resize with Photon's image-rs-backed samplers.
    pub fn photon_resize(&self, scale: f64, filter: &str) -> Result<WasmImage, JsValue> {
        let (width, height) =
            target_dimensions(self.width(), self.height(), scale).map_err(image_error)?;
        let filter = match filter {
            "nearest" => SamplingFilter::Nearest,
            "triangle" => SamplingFilter::Triangle,
            "catmull-rom" => SamplingFilter::CatmullRom,
            "gaussian" => SamplingFilter::Gaussian,
            "lanczos3" => SamplingFilter::Lanczos3,
            _ => {
                return Err(image_error(
                    "Photon filter must be nearest, triangle, catmull-rom, gaussian, or lanczos3",
                ));
            }
        };
        Ok(Self {
            inner: transform::resize(&self.inner, width, height, filter),
        })
    }

    /// Separable Lanczos scaling used by the calibrated browser recovery path.
    pub fn lanczos_resize(&self, scale: f64, radius: usize) -> Result<WasmImage, JsValue> {
        if !(2..=8).contains(&radius) {
            return Err(image_error("Lanczos radius must be between 2 and 8"));
        }
        let (target_width, target_height) =
            target_dimensions(self.width(), self.height(), scale).map_err(image_error)?;
        let source_width = self.width() as usize;
        let source_height = self.height() as usize;
        let target_width = target_width as usize;
        let target_height = target_height as usize;
        let source = self.raw();
        let mut horizontal = vec![0.0f32; target_width * source_height];
        for y in 0..source_height {
            for target_x in 0..target_width {
                let source_x = (target_x as f64 + 0.5) / scale - 0.5;
                let start = source_x.floor() as isize - radius as isize + 1;
                let mut sum = 0.0;
                let mut total = 0.0;
                for offset in 0..radius * 2 {
                    let requested_x = start + offset as isize;
                    let sample_x = requested_x.clamp(0, source_width as isize - 1) as usize;
                    let weight = lanczos(source_x - requested_x as f64, radius);
                    sum += f64::from(intensity(&source, y * source_width + sample_x)) * weight;
                    total += weight;
                }
                horizontal[y * target_width + target_x] = if total == 0.0 {
                    255.0
                } else {
                    (sum / total) as f32
                };
            }
        }

        let mut values = vec![255u8; target_width * target_height];
        for target_y in 0..target_height {
            let source_y = (target_y as f64 + 0.5) / scale - 0.5;
            let start = source_y.floor() as isize - radius as isize + 1;
            for x in 0..target_width {
                let mut sum = 0.0;
                let mut total = 0.0;
                for offset in 0..radius * 2 {
                    let requested_y = start + offset as isize;
                    let sample_y = requested_y.clamp(0, source_height as isize - 1) as usize;
                    let weight = lanczos(source_y - requested_y as f64, radius);
                    sum += f64::from(horizontal[sample_y * target_width + x]) * weight;
                    total += weight;
                }
                values[target_y * target_width + x] =
                    clamp_u8(if total == 0.0 { 255.0 } else { sum / total });
            }
        }
        Ok(Self::from_pixels(
            gray_pixels(&values),
            target_width as u32,
            target_height as u32,
        ))
    }

    pub fn auto_contrast(&self) -> WasmImage {
        let source = self.raw();
        let pixel_count = self.width() as usize * self.height() as usize;
        let mut histogram = [0usize; 256];
        for pixel in 0..pixel_count {
            histogram[intensity(&source, pixel) as usize] += 1;
        }
        let tail = (pixel_count as f64 * 0.01).floor().max(1.0) as usize;
        let mut low = 0usize;
        let mut high = 255usize;
        let mut cumulative = 0usize;
        while low < 255 && cumulative + histogram[low] < tail {
            cumulative += histogram[low];
            low += 1;
        }
        cumulative = 0;
        while high > 0 && cumulative + histogram[high] < tail {
            cumulative += histogram[high];
            high -= 1;
        }
        if high <= low + 4 {
            return Self::from_pixels(source, self.width(), self.height());
        }
        let factor = 255.0 / (high - low) as f64;
        let values = (0..pixel_count)
            .map(|pixel| clamp_u8((f64::from(intensity(&source, pixel)) - low as f64) * factor))
            .collect::<Vec<_>>();
        Self::from_pixels(gray_pixels(&values), self.width(), self.height())
    }

    pub fn gamma(&self, exponent: f64) -> Result<WasmImage, JsValue> {
        if !exponent.is_finite() || exponent <= 0.0 {
            return Err(image_error("gamma exponent must be positive and finite"));
        }
        let source = self.raw();
        let table = (0..256)
            .map(|value| clamp_u8(255.0 * (value as f64 / 255.0).powf(exponent)))
            .collect::<Vec<_>>();
        let values = (0..self.width() as usize * self.height() as usize)
            .map(|pixel| table[intensity(&source, pixel) as usize])
            .collect::<Vec<_>>();
        Ok(Self::from_pixels(
            gray_pixels(&values),
            self.width(),
            self.height(),
        ))
    }

    pub fn unsharp(&self, amount: f64) -> Result<WasmImage, JsValue> {
        if !amount.is_finite() || amount < 0.0 {
            return Err(image_error(
                "unsharp amount must be finite and non-negative",
            ));
        }
        let width = self.width() as usize;
        let height = self.height() as usize;
        let source = self.raw();
        let kernel = [1u32, 2, 1, 2, 4, 2, 1, 2, 1];
        let mut values = vec![0u8; width * height];
        for y in 0..height {
            for x in 0..width {
                let mut sum = 0u32;
                let mut weight = 0u32;
                for dy in -1isize..=1 {
                    for dx in -1isize..=1 {
                        let sample_x = (x as isize + dx).clamp(0, width as isize - 1) as usize;
                        let sample_y = (y as isize + dy).clamp(0, height as isize - 1) as usize;
                        let kernel_weight = kernel[((dy + 1) * 3 + dx + 1) as usize];
                        sum += u32::from(intensity(&source, sample_y * width + sample_x))
                            * kernel_weight;
                        weight += kernel_weight;
                    }
                }
                let original = f64::from(intensity(&source, y * width + x));
                let blurred = f64::from(sum) / f64::from(weight);
                values[y * width + x] = clamp_u8(original + amount * (original - blurred));
            }
        }
        Ok(Self::from_pixels(
            gray_pixels(&values),
            self.width(),
            self.height(),
        ))
    }

    pub fn otsu_level(&self) -> u8 {
        let source = self.raw();
        let pixel_count = self.width() as usize * self.height() as usize;
        let mut histogram = [0usize; 256];
        let mut total_intensity = 0usize;
        for pixel in 0..pixel_count {
            let value = intensity(&source, pixel) as usize;
            histogram[value] += 1;
            total_intensity += value;
        }
        let mut background_weight = 0usize;
        let mut background_sum = 0usize;
        let mut best_variance = -1.0f64;
        let mut best_level = 127u8;
        for (level, count) in histogram.iter().copied().enumerate() {
            background_weight += count;
            if background_weight == 0 {
                continue;
            }
            let foreground_weight = pixel_count - background_weight;
            if foreground_weight == 0 {
                break;
            }
            background_sum += level * count;
            let background_mean = background_sum as f64 / background_weight as f64;
            let foreground_mean =
                (total_intensity - background_sum) as f64 / foreground_weight as f64;
            let variance = background_weight as f64
                * foreground_weight as f64
                * (background_mean - foreground_mean).powi(2);
            if variance > best_variance {
                best_variance = variance;
                best_level = level as u8;
            }
        }
        best_level
    }

    /// Fixed thresholding is delegated to Photon. Adding one preserves the
    /// existing `value <= level` black-module convention.
    pub fn photon_threshold(&self, level: u8) -> WasmImage {
        let mut inner = self.inner.clone();
        monochrome::threshold(&mut inner, u32::from(level) + 1);
        Self { inner }
    }

    pub fn adaptive_threshold(&self, window_size: usize, bias: f64) -> Result<WasmImage, JsValue> {
        if !(3..=255).contains(&window_size) {
            return Err(image_error("adaptive window must be between 3 and 255"));
        }
        let width = self.width() as usize;
        let height = self.height() as usize;
        let source = self.raw();
        let integral_width = width + 1;
        let mut integral = vec![0f64; integral_width * (height + 1)];
        for y in 0..height {
            let mut row_sum = 0f64;
            for x in 0..width {
                row_sum += f64::from(intensity(&source, y * width + x));
                integral[(y + 1) * integral_width + x + 1] =
                    integral[y * integral_width + x + 1] + row_sum;
            }
        }
        let radius = (window_size / 2).max(1);
        let mut values = vec![255u8; width * height];
        for y in 0..height {
            let top = y.saturating_sub(radius);
            let bottom = (y + radius + 1).min(height);
            for x in 0..width {
                let left = x.saturating_sub(radius);
                let right = (x + radius + 1).min(width);
                let sum = integral[bottom * integral_width + right]
                    - integral[top * integral_width + right]
                    - integral[bottom * integral_width + left]
                    + integral[top * integral_width + left];
                let mean = sum / ((right - left) * (bottom - top)) as f64;
                values[y * width + x] =
                    if f64::from(intensity(&source, y * width + x)) <= mean - bias {
                        0
                    } else {
                        255
                    };
            }
        }
        Ok(Self::from_pixels(
            gray_pixels(&values),
            self.width(),
            self.height(),
        ))
    }

    pub fn gaussian_adaptive_threshold(
        &self,
        window_size: usize,
        bias: f64,
    ) -> Result<WasmImage, JsValue> {
        if !(3..=255).contains(&window_size) {
            return Err(image_error(
                "Gaussian adaptive window must be between 3 and 255",
            ));
        }
        let width = self.width() as usize;
        let height = self.height() as usize;
        let source = self.raw();
        let radius = (window_size / 2).max(1);
        let sigma = 0.3 * (radius as f64 - 1.0) + 0.8;
        let mut kernel = Vec::with_capacity(radius * 2 + 1);
        let mut kernel_sum = 0.0f64;
        for offset in -(radius as isize)..=radius as isize {
            let weight = (-(offset * offset) as f64 / (2.0 * sigma * sigma)).exp();
            kernel.push(weight as f32);
            kernel_sum += weight;
        }
        for weight in &mut kernel {
            *weight = (f64::from(*weight) / kernel_sum) as f32;
        }

        let mut horizontal = vec![0.0f32; width * height];
        for y in 0..height {
            for x in 0..width {
                let mut sum = 0.0f64;
                for offset in -(radius as isize)..=radius as isize {
                    let sample_x = (x as isize + offset).clamp(0, width as isize - 1) as usize;
                    sum += f64::from(intensity(&source, y * width + sample_x))
                        * f64::from(kernel[(offset + radius as isize) as usize]);
                }
                horizontal[y * width + x] = sum as f32;
            }
        }

        let mut values = vec![255u8; width * height];
        for y in 0..height {
            for x in 0..width {
                let mut mean = 0.0f64;
                for offset in -(radius as isize)..=radius as isize {
                    let sample_y = (y as isize + offset).clamp(0, height as isize - 1) as usize;
                    mean += f64::from(horizontal[sample_y * width + x])
                        * f64::from(kernel[(offset + radius as isize) as usize]);
                }
                values[y * width + x] =
                    if f64::from(intensity(&source, y * width + x)) > mean - bias {
                        255
                    } else {
                        0
                    };
            }
        }
        Ok(Self::from_pixels(
            gray_pixels(&values),
            self.width(),
            self.height(),
        ))
    }

    /// RGB inversion is delegated to Photon and preserves alpha.
    pub fn photon_invert(&self) -> WasmImage {
        let mut inner = self.inner.clone();
        channels::invert(&mut inner);
        Self { inner }
    }
}

#[wasm_bindgen]
pub fn fuse_mean_images(
    frames: &[u8],
    width: u32,
    height: u32,
    frame_count: usize,
) -> Result<WasmImage, JsValue> {
    if frame_count < 2 {
        return Err(image_error("fusion requires at least two frames"));
    }
    let frame_bytes = width as usize * height as usize * 4;
    validate_dimensions(width, height, frame_bytes).map_err(image_error)?;
    if frames.len() != frame_bytes * frame_count {
        return Err(image_error(
            "all fusion frames must have identical dimensions",
        ));
    }
    let pixel_count = width as usize * height as usize;
    let mut values = vec![0u8; pixel_count];
    for pixel in 0..pixel_count {
        let mut sum = 0usize;
        for frame in 0..frame_count {
            sum += frames[frame * frame_bytes + pixel * 4] as usize;
        }
        values[pixel] = clamp_u8(sum as f64 / frame_count as f64);
    }
    Ok(WasmImage::from_pixels(gray_pixels(&values), width, height))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(width: u32, height: u32, values: &[u8]) -> WasmImage {
        WasmImage::from_pixels(gray_pixels(values), width, height)
    }

    #[test]
    fn photon_threshold_and_invert_preserve_qr_conventions() {
        let source = image(3, 1, &[20, 100, 220]);
        let binary = source.photon_threshold(100);
        assert_eq!(binary.raw(), gray_pixels(&[0, 0, 255]));
        let inverted = binary.photon_invert();
        assert_eq!(inverted.raw(), gray_pixels(&[255, 255, 0]));
    }

    #[test]
    fn otsu_and_adaptive_threshold_separate_dark_modules() {
        let values = (0..21usize * 21)
            .map(|index| {
                let x = index % 21;
                let y = index / 21;
                let background = 80 + (x * 7) as u8;
                if (8..=12).contains(&x) && (8..=12).contains(&y) {
                    background - 35
                } else {
                    background
                }
            })
            .collect::<Vec<_>>();
        let source = image(21, 21, &values);
        let binary = source.adaptive_threshold(9, 5.0).unwrap();
        assert_eq!(intensity(&binary.raw(), 10 * 21 + 10), 0);
        assert_eq!(intensity(&binary.raw(), 2 * 21 + 18), 255);
        assert!(source.otsu_level() > 0);
    }

    #[test]
    fn custom_lanczos_and_photon_resize_return_requested_dimensions() {
        let source = image(21, 21, &vec![127; 21 * 21]);
        let calibrated = source.lanczos_resize(2.0, 4).unwrap();
        let photon = source.photon_resize(2.0, "lanczos3").unwrap();
        assert_eq!((calibrated.width(), calibrated.height()), (42, 42));
        assert_eq!((photon.width(), photon.height()), (42, 42));
    }

    #[test]
    fn fusion_averages_frames() {
        let mut frames = gray_pixels(&[0, 100]);
        frames.extend(gray_pixels(&[100, 200]));
        let fused = fuse_mean_images(&frames, 2, 1, 2).unwrap();
        assert_eq!(fused.raw(), gray_pixels(&[50, 150]));
    }
}
