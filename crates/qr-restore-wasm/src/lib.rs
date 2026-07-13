//! Image processing, QR Code Model 2 structure, and soft-decision primitives
//! for the browser.
//!
//! The compact ECC tables and function-pattern algorithms are adapted from
//! Project Nayuki's MIT-licensed QR Code generator. See the repository NOTICE.

mod image_ops;
mod recovery;

pub use image_ops::*;
pub use recovery::*;

use reed_solomon::{Decoder, Encoder};
use serde::Serialize;
use std::collections::BTreeMap;
use wasm_bindgen::prelude::*;

const EC_LEVELS: [char; 4] = ['L', 'M', 'Q', 'H'];
const ECC_CODEWORDS_PER_BLOCK: [[i16; 41]; 4] = [
    [
        -1, 7, 10, 15, 20, 26, 18, 20, 24, 30, 18, 20, 24, 26, 30, 22, 24, 28, 30, 28, 28, 28, 28,
        30, 30, 26, 28, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30,
    ],
    [
        -1, 10, 16, 26, 18, 24, 16, 18, 22, 22, 26, 30, 22, 22, 24, 24, 28, 28, 26, 26, 26, 26, 28,
        28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28,
    ],
    [
        -1, 13, 22, 18, 26, 18, 24, 18, 22, 20, 24, 28, 26, 24, 20, 30, 24, 28, 28, 26, 30, 28, 30,
        30, 30, 30, 28, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30,
    ],
    [
        -1, 17, 28, 22, 16, 22, 28, 26, 26, 24, 28, 24, 28, 22, 24, 24, 30, 28, 28, 26, 28, 30, 24,
        30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30,
    ],
];
const NUM_ERROR_CORRECTION_BLOCKS: [[i16; 41]; 4] = [
    [
        -1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 4, 4, 4, 4, 4, 6, 6, 6, 6, 7, 8, 8, 9, 9, 10, 12, 12, 12,
        13, 14, 15, 16, 17, 18, 19, 19, 20, 21, 22, 24, 25,
    ],
    [
        -1, 1, 1, 1, 2, 2, 4, 4, 4, 5, 5, 5, 8, 9, 9, 10, 10, 11, 13, 14, 16, 17, 17, 18, 20, 21,
        23, 25, 26, 28, 29, 31, 33, 35, 37, 38, 40, 43, 45, 47, 49,
    ],
    [
        -1, 1, 1, 2, 2, 4, 4, 6, 6, 8, 8, 8, 10, 12, 16, 12, 17, 16, 18, 21, 20, 23, 23, 25, 27,
        29, 34, 34, 35, 38, 40, 43, 45, 48, 51, 53, 56, 59, 62, 65, 68,
    ],
    [
        -1, 1, 1, 2, 4, 4, 4, 5, 6, 8, 8, 11, 11, 16, 16, 18, 16, 19, 21, 25, 25, 25, 34, 30, 32,
        35, 37, 40, 42, 45, 48, 51, 54, 57, 60, 63, 66, 70, 74, 77, 81,
    ],
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct BlockSpec {
    pub data_codewords: usize,
    pub ecc_codewords: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExtractedCodewords {
    pub codewords: Vec<u8>,
    pub bit_reliability: Vec<f32>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CorrectedBlock {
    pub codewords: Vec<u8>,
    pub corrected_symbols: usize,
    pub search_cost: f32,
}

#[derive(Clone, Debug, Serialize)]
pub struct CorrectedStream {
    pub codewords: Vec<u8>,
    pub corrected_symbols: usize,
    pub search_cost: f32,
}

type Deinterleaved = (Vec<Vec<u8>>, Vec<Vec<f32>>, Vec<BlockSpec>);

fn js_error(message: impl Into<String>) -> JsValue {
    JsValue::from_str(&message.into())
}

fn check_version(version: u8) -> Result<(), String> {
    if (1..=40).contains(&version) {
        Ok(())
    } else {
        Err("version must be between 1 and 40".into())
    }
}

fn ec_row(ec_level: &str) -> Result<usize, String> {
    let level = ec_level
        .chars()
        .next()
        .filter(|_| ec_level.len() == 1)
        .ok_or_else(|| "EC level must be L, M, Q, or H".to_string())?;
    EC_LEVELS
        .iter()
        .position(|candidate| *candidate == level)
        .ok_or_else(|| "EC level must be L, M, Q, or H".to_string())
}

fn ec_format_value(ec_level: &str) -> Result<u16, String> {
    match ec_level {
        "L" => Ok(1),
        "M" => Ok(0),
        "Q" => Ok(3),
        "H" => Ok(2),
        _ => Err("EC level must be L, M, Q, or H".into()),
    }
}

#[wasm_bindgen]
pub fn symbol_size(version: u8) -> Result<u16, JsValue> {
    symbol_size_native(version).map_err(js_error)
}

pub fn symbol_size_native(version: u8) -> Result<u16, String> {
    check_version(version)?;
    Ok(u16::from(version) * 4 + 17)
}

#[wasm_bindgen]
pub fn version_from_size(size: u16) -> Result<u8, JsValue> {
    version_from_size_native(size).map_err(js_error)
}

pub fn version_from_size_native(size: u16) -> Result<u8, String> {
    if size < 21 || !(size - 17).is_multiple_of(4) {
        return Err(format!("invalid QR Model 2 matrix size: {size}"));
    }
    let version = ((size - 17) / 4) as u8;
    check_version(version)?;
    Ok(version)
}

#[wasm_bindgen]
pub fn num_raw_data_modules(version: u8) -> Result<u32, JsValue> {
    num_raw_data_modules_native(version).map_err(js_error)
}

pub fn num_raw_data_modules_native(version: u8) -> Result<u32, String> {
    check_version(version)?;
    let version = u32::from(version);
    let mut result = (16 * version + 128) * version + 64;
    if version >= 2 {
        let align = version / 7 + 2;
        result -= (25 * align - 10) * align - 55;
        if version >= 7 {
            result -= 36;
        }
    }
    Ok(result)
}

pub fn block_layout_native(version: u8, ec_level: &str) -> Result<Vec<BlockSpec>, String> {
    check_version(version)?;
    let row = ec_row(ec_level)?;
    let ecc = ECC_CODEWORDS_PER_BLOCK[row][version as usize] as usize;
    let count = NUM_ERROR_CORRECTION_BLOCKS[row][version as usize] as usize;
    let raw = num_raw_data_modules_native(version)? as usize / 8;
    let short_total = raw / count;
    let num_short = count - raw % count;
    let short_data = short_total - ecc;
    Ok((0..count)
        .map(|index| BlockSpec {
            data_codewords: short_data + usize::from(index >= num_short),
            ecc_codewords: ecc,
        })
        .collect())
}

#[wasm_bindgen]
pub fn block_layout(version: u8, ec_level: &str) -> Result<JsValue, JsValue> {
    let value = block_layout_native(version, ec_level).map_err(js_error)?;
    serde_wasm_bindgen::to_value(&value).map_err(|error| js_error(error.to_string()))
}

pub fn alignment_positions_native(version: u8) -> Result<Vec<u16>, String> {
    check_version(version)?;
    if version == 1 {
        return Ok(Vec::new());
    }
    let count = usize::from(version / 7 + 2);
    let step = if version == 32 {
        26
    } else {
        ((u16::from(version) * 4 + count as u16 * 2 + 1) / (count as u16 * 2 - 2)) * 2
    };
    let mut result = vec![6];
    let mut position = symbol_size_native(version)? - 7;
    for index in 0..count - 1 {
        result.insert(1, position);
        if index + 1 < count - 1 {
            position -= step;
        }
    }
    Ok(result)
}

#[wasm_bindgen]
pub fn alignment_positions(version: u8) -> Result<Vec<u16>, JsValue> {
    alignment_positions_native(version).map_err(js_error)
}

#[wasm_bindgen]
pub fn format_bits(ec_level: &str, mask: u8) -> Result<u16, JsValue> {
    format_bits_native(ec_level, mask).map_err(js_error)
}

pub fn format_bits_native(ec_level: &str, mask: u8) -> Result<u16, String> {
    if mask > 7 {
        return Err("mask must be between 0 and 7".into());
    }
    let data = ec_format_value(ec_level)? << 3 | u16::from(mask);
    let mut remainder = data;
    for _ in 0..10 {
        remainder = (remainder << 1) ^ ((remainder >> 9) * 0x537);
    }
    Ok(((data << 10) | remainder) ^ 0x5412)
}

#[wasm_bindgen]
pub fn version_bits(version: u8) -> Result<u32, JsValue> {
    version_bits_native(version).map_err(js_error)
}

pub fn version_bits_native(version: u8) -> Result<u32, String> {
    check_version(version)?;
    let mut remainder = u32::from(version);
    for _ in 0..12 {
        remainder = (remainder << 1) ^ ((remainder >> 11) * 0x1f25);
    }
    Ok(u32::from(version) << 12 | remainder)
}

#[wasm_bindgen]
pub fn mask_applies(mask: u8, x: u16, y: u16) -> Result<bool, JsValue> {
    mask_applies_native(mask, x, y).map_err(js_error)
}

pub fn mask_applies_native(mask: u8, x: u16, y: u16) -> Result<bool, String> {
    let value = match mask {
        0 => (x + y) % 2,
        1 => y % 2,
        2 => x % 3,
        3 => (x + y) % 3,
        4 => (x / 3 + y / 2) % 2,
        5 => (x * y) % 2 + (x * y) % 3,
        6 => ((x * y) % 2 + (x * y) % 3) % 2,
        7 => ((x + y) % 2 + (x * y) % 3) % 2,
        _ => return Err("mask must be between 0 and 7".into()),
    };
    Ok(value == 0)
}

fn set_module(matrix: &mut [u8], functions: &mut [bool], size: usize, x: i32, y: i32, dark: bool) {
    if x >= 0 && y >= 0 && x < size as i32 && y < size as i32 {
        let index = y as usize * size + x as usize;
        matrix[index] = u8::from(dark);
        functions[index] = true;
    }
}

pub fn function_matrix_native(
    version: u8,
    ec_level: &str,
    mask: u8,
) -> Result<(Vec<u8>, Vec<bool>), String> {
    let size = symbol_size_native(version)? as usize;
    let mut matrix = vec![0; size * size];
    let mut functions = vec![false; size * size];

    for (center_x, center_y) in [(3, 3), (size as i32 - 4, 3), (3, size as i32 - 4)] {
        for dy in -4i32..=4 {
            for dx in -4i32..=4 {
                let distance = dx.abs().max(dy.abs());
                set_module(
                    &mut matrix,
                    &mut functions,
                    size,
                    center_x + dx,
                    center_y + dy,
                    distance != 2 && distance != 4,
                );
            }
        }
    }

    for index in 8..size - 8 {
        let dark = index % 2 == 0;
        set_module(&mut matrix, &mut functions, size, 6, index as i32, dark);
        set_module(&mut matrix, &mut functions, size, index as i32, 6, dark);
    }

    let positions = alignment_positions_native(version)?;
    let last = positions.len().saturating_sub(1);
    for (row, center_y) in positions.iter().enumerate() {
        for (col, center_x) in positions.iter().enumerate() {
            if [(0, 0), (0, last), (last, 0)].contains(&(row, col)) {
                continue;
            }
            for dy in -2i32..=2 {
                for dx in -2i32..=2 {
                    let dark = dx.abs().max(dy.abs()) != 1;
                    set_module(
                        &mut matrix,
                        &mut functions,
                        size,
                        i32::from(*center_x) + dx,
                        i32::from(*center_y) + dy,
                        dark,
                    );
                }
            }
        }
    }

    let bits = format_bits_native(ec_level, mask)?;
    for index in 0..6 {
        set_module(
            &mut matrix,
            &mut functions,
            size,
            8,
            index,
            bits >> index & 1 != 0,
        );
    }
    set_module(&mut matrix, &mut functions, size, 8, 7, bits >> 6 & 1 != 0);
    set_module(&mut matrix, &mut functions, size, 8, 8, bits >> 7 & 1 != 0);
    set_module(&mut matrix, &mut functions, size, 7, 8, bits >> 8 & 1 != 0);
    for index in 9..15 {
        set_module(
            &mut matrix,
            &mut functions,
            size,
            14 - index,
            8,
            bits >> index & 1 != 0,
        );
    }
    for index in 0..8 {
        set_module(
            &mut matrix,
            &mut functions,
            size,
            size as i32 - 1 - index,
            8,
            bits >> index & 1 != 0,
        );
    }
    for index in 8..15 {
        set_module(
            &mut matrix,
            &mut functions,
            size,
            8,
            size as i32 - 15 + index,
            bits >> index & 1 != 0,
        );
    }
    set_module(&mut matrix, &mut functions, size, 8, size as i32 - 8, true);

    if version >= 7 {
        let bits = version_bits_native(version)?;
        for index in 0..18 {
            let dark = bits >> index & 1 != 0;
            let a = size as i32 - 11 + index % 3;
            let b = index / 3;
            set_module(&mut matrix, &mut functions, size, a, b, dark);
            set_module(&mut matrix, &mut functions, size, b, a, dark);
        }
    }
    Ok((matrix, functions))
}

/// Each returned byte is 0 for a data module, 1 for a light function module,
/// and 2 for a dark function module.
#[wasm_bindgen]
pub fn function_modules(version: u8, ec_level: &str, mask: u8) -> Result<Vec<u8>, JsValue> {
    let (matrix, functions) = function_matrix_native(version, ec_level, mask).map_err(js_error)?;
    Ok(matrix
        .into_iter()
        .zip(functions)
        .map(|(dark, function)| if function { dark + 1 } else { 0 })
        .collect())
}

pub fn data_coordinates_native(
    version: u8,
    ec_level: &str,
    mask: u8,
) -> Result<Vec<(u16, u16)>, String> {
    let (_, functions) = function_matrix_native(version, ec_level, mask)?;
    let size = symbol_size_native(version)? as usize;
    let mut result = Vec::new();
    let mut right = size - 1;
    while right >= 1 {
        if right == 6 {
            right = 5;
        }
        let upward = ((right + 1) & 2) == 0;
        for vertical in 0..size {
            let y = if upward {
                size - 1 - vertical
            } else {
                vertical
            };
            for offset in 0..2 {
                let x = right - offset;
                if !functions[y * size + x] {
                    result.push((x as u16, y as u16));
                }
            }
        }
        if right < 2 {
            break;
        }
        right -= 2;
    }
    Ok(result)
}

/// Flattened x,y pairs in standard QR data traversal order.
#[wasm_bindgen]
pub fn data_coordinates(version: u8, ec_level: &str, mask: u8) -> Result<Vec<u16>, JsValue> {
    Ok(data_coordinates_native(version, ec_level, mask)
        .map_err(js_error)?
        .into_iter()
        .flat_map(|(x, y)| [x, y])
        .collect())
}

pub fn extract_codewords_native(
    modules: &[u8],
    confidence: &[f32],
    version: u8,
    ec_level: &str,
    mask: u8,
) -> Result<ExtractedCodewords, String> {
    let size = symbol_size_native(version)? as usize;
    if modules.len() != size * size {
        return Err("module matrix length does not match version".into());
    }
    if confidence.len() != modules.len() {
        return Err("confidence length does not match module matrix".into());
    }
    let usable_bits = num_raw_data_modules_native(version)? as usize / 8 * 8;
    let mut bits = Vec::with_capacity(usable_bits);
    let mut reliability = Vec::with_capacity(usable_bits);
    for (x, y) in data_coordinates_native(version, ec_level, mask)?
        .into_iter()
        .take(usable_bits)
    {
        let index = y as usize * size + x as usize;
        let bit = (modules[index] != 0) ^ mask_applies_native(mask, x, y)?;
        bits.push(u8::from(bit));
        reliability.push(confidence[index].clamp(0.0, 1.0));
    }
    let codewords = bits
        .chunks_exact(8)
        .map(|chunk| chunk.iter().fold(0u8, |value, bit| value << 1 | bit))
        .collect();
    Ok(ExtractedCodewords {
        codewords,
        bit_reliability: reliability,
    })
}

#[wasm_bindgen]
pub fn extract_codewords(
    modules: &[u8],
    confidence: &[f32],
    version: u8,
    ec_level: &str,
    mask: u8,
) -> Result<JsValue, JsValue> {
    let value =
        extract_codewords_native(modules, confidence, version, ec_level, mask).map_err(js_error)?;
    serde_wasm_bindgen::to_value(&value).map_err(|error| js_error(error.to_string()))
}

pub fn matrix_from_codewords_native(
    codewords: &[u8],
    version: u8,
    ec_level: &str,
    mask: u8,
) -> Result<Vec<u8>, String> {
    let expected = num_raw_data_modules_native(version)? as usize / 8;
    if codewords.len() != expected {
        return Err(format!(
            "expected {expected} codewords, got {}",
            codewords.len()
        ));
    }
    let (mut matrix, _) = function_matrix_native(version, ec_level, mask)?;
    let size = symbol_size_native(version)? as usize;
    for (bit_index, (x, y)) in data_coordinates_native(version, ec_level, mask)?
        .into_iter()
        .enumerate()
    {
        let bit = if bit_index < codewords.len() * 8 {
            codewords[bit_index >> 3] >> (7 - (bit_index & 7)) & 1 != 0
        } else {
            false
        };
        matrix[y as usize * size + x as usize] = u8::from(bit ^ mask_applies_native(mask, x, y)?);
    }
    Ok(matrix)
}

#[wasm_bindgen]
pub fn matrix_from_codewords(
    codewords: &[u8],
    version: u8,
    ec_level: &str,
    mask: u8,
) -> Result<Vec<u8>, JsValue> {
    matrix_from_codewords_native(codewords, version, ec_level, mask).map_err(js_error)
}

fn deinterleave_native(
    codewords: &[u8],
    bit_reliability: &[f32],
    version: u8,
    ec_level: &str,
) -> Result<Deinterleaved, String> {
    if bit_reliability.len() != codewords.len() * 8 {
        return Err("bit reliability must contain eight values per codeword".into());
    }
    let specs = block_layout_native(version, ec_level)?;
    let expected: usize = specs
        .iter()
        .map(|spec| spec.data_codewords + spec.ecc_codewords)
        .sum();
    if codewords.len() != expected {
        return Err("codeword count does not match QR block layout".into());
    }
    let mut blocks: Vec<Vec<u8>> = specs
        .iter()
        .map(|spec| vec![0; spec.data_codewords + spec.ecc_codewords])
        .collect();
    let mut reliabilities: Vec<Vec<f32>> = blocks
        .iter()
        .map(|block| vec![0.0; block.len() * 8])
        .collect();
    let mut cursor = 0usize;
    let max_data = specs
        .iter()
        .map(|spec| spec.data_codewords)
        .max()
        .unwrap_or(0);
    for data_index in 0..max_data {
        for (block_index, spec) in specs.iter().enumerate() {
            if data_index < spec.data_codewords {
                blocks[block_index][data_index] = codewords[cursor];
                reliabilities[block_index][data_index * 8..data_index * 8 + 8]
                    .copy_from_slice(&bit_reliability[cursor * 8..cursor * 8 + 8]);
                cursor += 1;
            }
        }
    }
    for ecc_index in 0..specs[0].ecc_codewords {
        for (block_index, spec) in specs.iter().enumerate() {
            let target = spec.data_codewords + ecc_index;
            blocks[block_index][target] = codewords[cursor];
            reliabilities[block_index][target * 8..target * 8 + 8]
                .copy_from_slice(&bit_reliability[cursor * 8..cursor * 8 + 8]);
            cursor += 1;
        }
    }
    Ok((blocks, reliabilities, specs))
}

fn interleave_native(blocks: &[Vec<u8>], specs: &[BlockSpec]) -> Result<Vec<u8>, String> {
    if blocks.len() != specs.len() || blocks.is_empty() {
        return Err("block count does not match layout".into());
    }
    let mut result = Vec::new();
    let max_data = specs
        .iter()
        .map(|spec| spec.data_codewords)
        .max()
        .unwrap_or(0);
    for data_index in 0..max_data {
        for (block, spec) in blocks.iter().zip(specs) {
            if data_index < spec.data_codewords {
                result.push(block[data_index]);
            }
        }
    }
    for ecc_index in 0..specs[0].ecc_codewords {
        for (block, spec) in blocks.iter().zip(specs) {
            result.push(block[spec.data_codewords + ecc_index]);
        }
    }
    Ok(result)
}

fn read_data_bits(data: &[u8], cursor: &mut usize, count: usize) -> Option<usize> {
    if count > usize::BITS as usize || *cursor + count > data.len() * 8 {
        return None;
    }
    let mut value = 0usize;
    for _ in 0..count {
        value = value << 1 | usize::from((data[*cursor / 8] >> (7 - *cursor % 8)) & 1);
        *cursor += 1;
    }
    Some(value)
}

fn character_count_bits(mode: usize, version: u8) -> Option<usize> {
    let group = if version <= 9 {
        0
    } else if version <= 26 {
        1
    } else {
        2
    };
    match mode {
        0x1 => Some([10, 12, 14][group]),
        0x2 => Some([9, 11, 13][group]),
        0x4 => Some([8, 16, 16][group]),
        0x8 | 0xd => Some([8, 10, 12][group]),
        _ => None,
    }
}

fn segment_data_bits(mode: usize, count: usize) -> Option<usize> {
    match mode {
        0x1 => Some(count / 3 * 10 + [0, 4, 7][count % 3]),
        0x2 => Some(count / 2 * 11 + count % 2 * 6),
        0x4 => count.checked_mul(8),
        0x8 | 0xd => count.checked_mul(13),
        _ => None,
    }
}

fn canonicalize_qr_data_tail(data: &[u8], version: u8) -> Option<Vec<u8>> {
    let mut cursor = 0usize;
    let mut saw_data = false;
    let terminator_start = loop {
        let mode_start = cursor;
        let mode = read_data_bits(data, &mut cursor, 4)?;
        match mode {
            0x0 if saw_data => break mode_start,
            0x1 | 0x2 | 0x4 | 0x8 => {
                let count =
                    read_data_bits(data, &mut cursor, character_count_bits(mode, version)?)?;
                let data_bits = segment_data_bits(mode, count)?;
                if cursor + data_bits > data.len() * 8 {
                    return None;
                }
                cursor += data_bits;
                saw_data = true;
            }
            // Structured append, FNC1, ECI, and Hanzi headers can precede
            // ordinary payload segments. Their fixed/encoded metadata is
            // preserved exactly; only the terminator and padding are rebuilt.
            0x3 => {
                read_data_bits(data, &mut cursor, 16)?;
            }
            0x5 => {}
            0x7 => {
                let first = read_data_bits(data, &mut cursor, 8)?;
                if first & 0x80 == 0 {
                    // One-byte ECI designator.
                } else if first & 0xc0 == 0x80 {
                    read_data_bits(data, &mut cursor, 8)?;
                } else if first & 0xe0 == 0xc0 {
                    read_data_bits(data, &mut cursor, 16)?;
                } else {
                    return None;
                }
            }
            0x9 => {
                read_data_bits(data, &mut cursor, 8)?;
            }
            0xd => {
                read_data_bits(data, &mut cursor, 4)?;
                let count =
                    read_data_bits(data, &mut cursor, character_count_bits(mode, version)?)?;
                let data_bits = segment_data_bits(mode, count)?;
                if cursor + data_bits > data.len() * 8 {
                    return None;
                }
                cursor += data_bits;
                saw_data = true;
            }
            _ => return None,
        }
    };

    let total_bits = data.len() * 8;
    if terminator_start < 12 || terminator_start + 4 > total_bits {
        return None;
    }
    let mut repaired = data.to_vec();
    for bit in terminator_start..total_bits {
        repaired[bit / 8] &= !(1 << (7 - bit % 8));
    }
    let pad_start = (terminator_start + 4).div_ceil(8);
    for (index, value) in repaired[pad_start..].iter_mut().enumerate() {
        *value = if index % 2 == 0 { 0xec } else { 0x11 };
    }
    Some(repaired)
}

/// Rebuild canonical terminator/padding and RS parity when a geometrically
/// deformed symbol preserves its complete segment payload but damages the tail.
pub fn repair_data_tail_native(
    codewords: &[u8],
    version: u8,
    ec_level: &str,
) -> Result<Option<CorrectedStream>, String> {
    let reliability = vec![1.0; codewords.len() * 8];
    let (blocks, _, specs) = deinterleave_native(codewords, &reliability, version, ec_level)?;
    let data = blocks
        .iter()
        .zip(&specs)
        .flat_map(|(block, spec)| block[..spec.data_codewords].iter().copied())
        .collect::<Vec<_>>();
    let Some(repaired_data) = canonicalize_qr_data_tail(&data, version) else {
        return Ok(None);
    };
    let mut cursor = 0usize;
    let mut repaired_blocks = Vec::with_capacity(specs.len());
    for spec in &specs {
        let end = cursor + spec.data_codewords;
        let encoded = Encoder::new(spec.ecc_codewords).encode(&repaired_data[cursor..end]);
        repaired_blocks.push(encoded[..].to_vec());
        cursor = end;
    }
    let repaired = interleave_native(&repaired_blocks, &specs)?;
    if repaired == codewords {
        return Ok(None);
    }
    let corrected_symbols = codewords
        .iter()
        .zip(&repaired)
        .filter(|(left, right)| left != right)
        .count();
    Ok(Some(CorrectedStream {
        codewords: repaired,
        corrected_symbols,
        search_cost: corrected_symbols as f32 + 6.0,
    }))
}

fn try_rs_candidate(
    results: &mut BTreeMap<Vec<u8>, CorrectedBlock>,
    candidate: &[u8],
    ecc_len: usize,
    erasures: Option<&[u8]>,
    flip_cost: usize,
) {
    let decoder = Decoder::new(ecc_len);
    // reed-solomon 0.2 contains a debug assertion for a degenerate error
    // locator. Treat that malformed hypothesis as an ordinary failed decode;
    // optimized WASM builds already return an invalid result for the same case.
    let Ok(Ok((corrected, count))) =
        std::panic::catch_unwind(|| decoder.correct_err_count(candidate, erasures))
    else {
        return;
    };
    if decoder.is_corrupted(&corrected) {
        return;
    }
    let codewords = corrected[..].to_vec();
    let value = CorrectedBlock {
        codewords: codewords.clone(),
        corrected_symbols: count,
        search_cost: count as f32 + flip_cost as f32 * 0.5,
    };
    match results.get(&codewords) {
        Some(previous) if previous.search_cost <= value.search_cost => {}
        _ => {
            results.insert(codewords, value);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn perform_attempt(
    results: &mut BTreeMap<Vec<u8>, CorrectedBlock>,
    attempts: &mut usize,
    max_attempts: usize,
    max_results: usize,
    candidate: &[u8],
    ecc_len: usize,
    erasures: Option<&[u8]>,
    flip_cost: usize,
) -> bool {
    if *attempts >= max_attempts {
        return false;
    }
    *attempts += 1;
    try_rs_candidate(results, candidate, ecc_len, erasures, flip_cost);
    *attempts < max_attempts && results.len() < max_results
}

fn combinations(
    positions: &[(usize, usize)],
    count: usize,
    start: usize,
    selected: &mut Vec<(usize, usize)>,
    callback: &mut impl FnMut(&[(usize, usize)]) -> bool,
) -> bool {
    if selected.len() == count {
        return callback(selected);
    }
    let remaining = count - selected.len();
    for index in start..=positions.len().saturating_sub(remaining) {
        selected.push(positions[index]);
        if !combinations(positions, count, index + 1, selected, callback) {
            return false;
        }
        selected.pop();
    }
    true
}

pub fn correct_block_candidates_native(
    block: &[u8],
    bit_reliability: &[f32],
    ecc_len: usize,
    uncertain_bits: usize,
    max_attempts: usize,
    max_results: usize,
) -> Result<(Vec<CorrectedBlock>, usize), String> {
    if block.is_empty() || block.len() > 255 || ecc_len == 0 || ecc_len >= block.len() {
        return Err("invalid Reed-Solomon block dimensions".into());
    }
    if bit_reliability.len() != block.len() * 8 {
        return Err("bit reliability must contain eight values per codeword".into());
    }
    let mut results = BTreeMap::new();
    let mut attempts = 0usize;
    perform_attempt(
        &mut results,
        &mut attempts,
        max_attempts,
        max_results,
        block,
        ecc_len,
        None,
        0,
    );
    let (minimum_reliability, maximum_reliability) = bit_reliability.iter().copied().fold(
        (f32::INFINITY, f32::NEG_INFINITY),
        |(minimum, maximum), value| (minimum.min(value), maximum.max(value)),
    );
    if maximum_reliability - minimum_reliability > f32::EPSILON {
        let mut byte_order: Vec<usize> = (0..block.len()).collect();
        byte_order.sort_by(|left, right| {
            let left_value = bit_reliability[left * 8..left * 8 + 8]
                .iter()
                .copied()
                .fold(1.0f32, f32::min);
            let right_value = bit_reliability[right * 8..right * 8 + 8]
                .iter()
                .copied()
                .fold(1.0f32, f32::min);
            left_value.total_cmp(&right_value)
        });
        for count in 1..=ecc_len.min(block.len()) {
            let erasures: Vec<u8> = byte_order[..count]
                .iter()
                .map(|value| *value as u8)
                .collect();
            if !perform_attempt(
                &mut results,
                &mut attempts,
                max_attempts,
                max_results,
                block,
                ecc_len,
                Some(&erasures),
                0,
            ) {
                break;
            }
        }
    }

    if uncertain_bits > 0 && attempts < max_attempts && results.len() < max_results {
        let mut bit_order: Vec<usize> = (0..bit_reliability.len()).collect();
        bit_order.sort_by(|left, right| bit_reliability[*left].total_cmp(&bit_reliability[*right]));
        let positions: Vec<(usize, usize)> = bit_order
            .into_iter()
            .take(uncertain_bits)
            .map(|index| (index / 8, index % 8))
            .collect();
        let max_flips = if uncertain_bits > 12 { 4 } else { 3 };
        for count in 1..=max_flips.min(positions.len()) {
            let mut selected = Vec::new();
            let keep_going = combinations(&positions, count, 0, &mut selected, &mut |selection| {
                let mut mutated = block.to_vec();
                for (byte_index, bit_index) in selection {
                    mutated[*byte_index] ^= 1 << (7 - bit_index);
                }
                perform_attempt(
                    &mut results,
                    &mut attempts,
                    max_attempts,
                    max_results,
                    &mutated,
                    ecc_len,
                    None,
                    selection.len(),
                )
            });
            if !keep_going {
                break;
            }
        }
    }

    let mut ordered: Vec<_> = results.into_values().collect();
    ordered.sort_by(|left, right| {
        left.search_cost
            .total_cmp(&right.search_cost)
            .then_with(|| left.codewords.cmp(&right.codewords))
    });
    ordered.truncate(max_results);
    Ok((ordered, attempts))
}

#[wasm_bindgen]
pub fn correct_block_candidates(
    block: &[u8],
    bit_reliability: &[f32],
    ecc_len: usize,
    uncertain_bits: usize,
    max_attempts: usize,
    max_results: usize,
) -> Result<JsValue, JsValue> {
    let (candidates, attempts) = correct_block_candidates_native(
        block,
        bit_reliability,
        ecc_len,
        uncertain_bits,
        max_attempts,
        max_results,
    )
    .map_err(js_error)?;
    serde_wasm_bindgen::to_value(&(candidates, attempts))
        .map_err(|error| js_error(error.to_string()))
}

fn collect_stream_combinations(
    per_block: &[Vec<CorrectedBlock>],
    specs: &[BlockSpec],
    index: usize,
    selection: &mut Vec<CorrectedBlock>,
    output: &mut Vec<CorrectedStream>,
    limit: usize,
) -> Result<(), String> {
    if output.len() >= limit {
        return Ok(());
    }
    if index == per_block.len() {
        let blocks: Vec<Vec<u8>> = selection
            .iter()
            .map(|candidate| candidate.codewords.clone())
            .collect();
        output.push(CorrectedStream {
            codewords: interleave_native(&blocks, specs)?,
            corrected_symbols: selection
                .iter()
                .map(|candidate| candidate.corrected_symbols)
                .sum(),
            search_cost: selection
                .iter()
                .map(|candidate| candidate.search_cost)
                .sum(),
        });
        return Ok(());
    }
    for candidate in &per_block[index] {
        selection.push(candidate.clone());
        collect_stream_combinations(per_block, specs, index + 1, selection, output, limit)?;
        selection.pop();
        if output.len() >= limit {
            break;
        }
    }
    Ok(())
}

pub fn correct_interleaved_native(
    codewords: &[u8],
    bit_reliability: &[f32],
    version: u8,
    ec_level: &str,
    uncertain_bits: usize,
    max_attempts: usize,
    max_combinations: usize,
) -> Result<(Vec<CorrectedStream>, usize), String> {
    let (blocks, reliabilities, specs) =
        deinterleave_native(codewords, bit_reliability, version, ec_level)?;
    let mut attempts = 0usize;
    let mut per_block = Vec::with_capacity(blocks.len());
    for ((block, reliability), spec) in blocks.iter().zip(&reliabilities).zip(&specs) {
        let remaining = max_attempts.saturating_sub(attempts).max(1);
        let (candidates, used) = correct_block_candidates_native(
            block,
            reliability,
            spec.ecc_codewords,
            uncertain_bits,
            remaining,
            3,
        )?;
        attempts += used;
        if candidates.is_empty() {
            return Ok((Vec::new(), attempts));
        }
        per_block.push(candidates);
    }
    let mut output = Vec::new();
    collect_stream_combinations(
        &per_block,
        &specs,
        0,
        &mut Vec::new(),
        &mut output,
        max_combinations,
    )?;
    output.sort_by(|left, right| {
        left.search_cost
            .total_cmp(&right.search_cost)
            .then_with(|| left.codewords.cmp(&right.codewords))
    });
    Ok((output, attempts))
}

#[wasm_bindgen]
pub fn correct_interleaved(
    codewords: &[u8],
    bit_reliability: &[f32],
    version: u8,
    ec_level: &str,
    uncertain_bits: usize,
    max_attempts: usize,
    max_combinations: usize,
) -> Result<JsValue, JsValue> {
    let value = correct_interleaved_native(
        codewords,
        bit_reliability,
        version,
        ec_level,
        uncertain_bits,
        max_attempts,
        max_combinations,
    )
    .map_err(js_error)?;
    serde_wasm_bindgen::to_value(&value).map_err(|error| js_error(error.to_string()))
}

#[wasm_bindgen]
pub fn binary_entropy(probability: f64) -> f64 {
    let probability = probability.clamp(f64::EPSILON, 1.0 - f64::EPSILON);
    -probability * probability.log2() - (1.0 - probability) * (1.0 - probability).log2()
}

/// Combine frame-wise log-likelihood ratios. Samples are frame-major and each
/// weight applies to one complete module matrix.
#[wasm_bindgen]
pub fn combine_module_llrs(
    samples: &[f32],
    weights: &[f32],
    module_count: usize,
) -> Result<Vec<f32>, JsValue> {
    if module_count == 0 || samples.len() != weights.len() * module_count {
        return Err(js_error(
            "samples must contain one module matrix per weight",
        ));
    }
    let mut combined = vec![0.0f32; module_count];
    let mut total_weight = 0.0f32;
    for (frame, weight) in weights.iter().copied().enumerate() {
        let weight = weight.max(0.0);
        total_weight += weight;
        for module in 0..module_count {
            combined[module] += samples[frame * module_count + module] * weight;
        }
    }
    if total_weight > 0.0 {
        for value in &mut combined {
            *value /= total_weight;
        }
    }
    Ok(combined)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reed_solomon::Encoder;

    #[test]
    fn known_format_values_match_qr_standard() {
        assert_eq!(format_bits_native("L", 0).unwrap(), 0x77c4);
        assert_eq!(format_bits_native("M", 2).unwrap(), 0x5e7c);
        let mut values = std::collections::BTreeSet::new();
        for level in EC_LEVELS {
            for mask in 0..8 {
                values.insert(format_bits_native(&level.to_string(), mask).unwrap());
            }
        }
        assert_eq!(values.len(), 32);
    }

    #[test]
    fn every_version_and_ec_layout_has_the_expected_length() {
        for version in 1..=40 {
            for level in EC_LEVELS {
                let layout = block_layout_native(version, &level.to_string()).unwrap();
                let total: usize = layout
                    .iter()
                    .map(|spec| spec.data_codewords + spec.ecc_codewords)
                    .sum();
                assert_eq!(
                    total,
                    num_raw_data_modules_native(version).unwrap() as usize / 8,
                    "version={version}, level={level}"
                );
            }
        }
    }

    #[test]
    fn matrix_codewords_round_trip_for_all_versions() {
        // Golden hashes lock the function-map, traversal, interleaving, and
        // masking behavior across every Model 2 version and EC level.
        let expected_hashes = [
            0xe7b84d07b0ac8094,
            0xdea2d5644017ea43,
            0x1d31964f7ad8e196,
            0x136bbcc7527fa689,
            0x4c823f0d8782e831,
            0xff0bbd5ef72f8087,
            0xd7a7c47ef0e6ed57,
            0x0b4e964e8c99460a,
            0x42d679bfad46be70,
            0x2f2c11b64d22a736,
            0x06ca0cf98079cc69,
            0x55f1cbb2134f57d2,
            0x6f192345698beef3,
            0xa5985236c623b8dd,
            0xb38d77f40194f059,
            0x6e3ada22ab669b76,
            0x71ec4ef122145fdf,
            0x84f728821027ba9d,
            0x87edf2493c80914a,
            0x5f7686412a8b9b5b,
            0xd7a5af066b4c6a2c,
            0xb294215982692e24,
            0x1a78f926272fb667,
            0xfff7a249dbea5af8,
            0xc95317d2d32add3a,
            0x7448e6ab3b31d508,
            0x6541302df7c8ae5f,
            0x8d8b563936c438f1,
            0x5dedd3171fc116ce,
            0x0a7c5cd1a8080176,
            0x3d76eebcd04dc323,
            0x2a9266de433f350b,
            0xad742498b3e0c162,
            0xf1d7a6ff274ea16a,
            0x1d0cfafdf5d07469,
            0xd34c804a54e41280,
            0x0d2fc288ade5b717,
            0x4abd6370896d347a,
            0xa97889713c3ba5f4,
            0xcd28f50654064406,
        ];
        for version in 1..=40 {
            let level = EC_LEVELS[version as usize % EC_LEVELS.len()].to_string();
            let mask = version % 8;
            let count = num_raw_data_modules_native(version).unwrap() as usize / 8;
            let codewords: Vec<u8> = (0..count).map(|index| index as u8).collect();
            let matrix = matrix_from_codewords_native(&codewords, version, &level, mask).unwrap();
            let hash = matrix.iter().fold(0xcbf29ce484222325u64, |hash, byte| {
                (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
            });
            assert_eq!(hash, expected_hashes[version as usize - 1]);
            let confidence = vec![1.0; matrix.len()];
            let extracted =
                extract_codewords_native(&matrix, &confidence, version, &level, mask).unwrap();
            assert_eq!(extracted.codewords, codewords);
        }
    }

    #[test]
    fn errors_and_erasures_are_corrected() {
        let encoded = Encoder::new(18).encode(b"soft decision");
        let original = encoded[..].to_vec();
        assert_eq!(
            original,
            vec![
                0x73, 0x6f, 0x66, 0x74, 0x20, 0x64, 0x65, 0x63, 0x69, 0x73, 0x69, 0x6f, 0x6e, 0x13,
                0x59, 0x1c, 0x46, 0xb5, 0x09, 0x4a, 0xf5, 0x17, 0xaf, 0xa3, 0xcc, 0x49, 0x7a, 0x76,
                0xbe, 0x83, 0x82,
            ]
        );
        let mut damaged = original.clone();
        damaged[0] ^= 0x55;
        damaged[3] ^= 0xa1;
        let mut reliability = vec![1.0; damaged.len() * 8];
        reliability[..8].fill(0.001);
        reliability[24..32].fill(0.002);
        let (candidates, attempts) =
            correct_block_candidates_native(&damaged, &reliability, 18, 8, 2_000, 3).unwrap();
        assert!(attempts > 0);
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.codewords == original)
        );
    }

    #[test]
    fn canonical_tail_repair_preserves_deformed_payload_bits() {
        fn hex(value: &str) -> Vec<u8> {
            value
                .as_bytes()
                .chunks_exact(2)
                .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                .collect()
        }

        let damaged = hex(
            "41a68747470733a2f2f7777772e64796e616d736f66742e636f6d2f0ec11fdb9ec11ef5241fffe18ca3c561d",
        );
        let expected = hex(
            "41a68747470733a2f2f7777772e64796e616d736f66742e636f6d2f0ec11ec11ec11ef52e5ffff51ea26abe3",
        );
        let repaired = repair_data_tail_native(&damaged, 2, "L")
            .unwrap()
            .expect("complete segment should permit deterministic tail repair");
        assert_eq!(repaired.codewords, expected);
        assert!(repaired.corrected_symbols > 0);
    }
}
