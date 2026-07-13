//! Rust-owned browser recovery engine.
//!
//! JavaScript uploads decoded RGBA observations and receives progress/results.
//! Variant planning, image processing, QR localization/decoding, correction,
//! verification, fusion, ranking, and ambiguity decisions stay in this module.

use crate::{
    WasmImage, correct_interleaved_native, extract_codewords_native, format_bits_native,
    function_matrix_native, matrix_from_codewords_native, repair_data_tail_native,
};
use js_sys::Function;
use regex::Regex;
use rqrr::{BitGrid, Grid, PreparedImage, SimpleGrid};
use rxing::{
    BinaryBitmap, DecodeHints, Luma8LuminanceSource,
    common::{DetectorRXingResult, HybridBinarizer, PerspectiveTransform, Quadrilateral},
    point,
    qrcode::{
        cpp_port::detector::{FindFinderPatterns, GenerateFinderPatternSets, SampleQR},
        detector::Detector,
    },
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, btree_map::Entry};
use wasm_bindgen::prelude::*;

#[cfg(target_arch = "wasm32")]
fn clock_seconds() -> f64 {
    js_sys::Date::now() / 1_000.0
}

#[cfg(not(target_arch = "wasm32"))]
fn clock_seconds() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |duration| duration.as_secs_f64())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Effort {
    Fast,
    Balanced,
    Thorough,
}

impl Effort {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "fast" => Ok(Self::Fast),
            "balanced" => Ok(Self::Balanced),
            "thorough" => Ok(Self::Thorough),
            _ => Err("effort must be fast, balanced, or thorough".into()),
        }
    }
}

#[derive(Clone, Copy)]
struct EffortProfile {
    variant_limit: usize,
    scales: &'static [f64],
    channels: &'static [&'static str],
    adaptive: &'static [(usize, f64)],
    candidate_attempts: usize,
    chase_bits: usize,
    confirmation_reads: usize,
    confirmation_window: usize,
    default_seconds: f64,
}

fn effort_profile(effort: Effort) -> EffortProfile {
    match effort {
        Effort::Fast => EffortProfile {
            variant_limit: 20,
            scales: &[1.0, 2.0],
            channels: &["luma"],
            adaptive: &[(21, 5.0), (35, 8.0)],
            candidate_attempts: 2_000,
            chase_bits: 0,
            confirmation_reads: 2,
            confirmation_window: 4,
            default_seconds: 10.0,
        },
        Effort::Balanced => EffortProfile {
            variant_limit: 102,
            scales: &[1.0, 2.0, 3.0, 4.0],
            channels: &["luma", "green", "red", "blue"],
            adaptive: &[(15, 3.0), (25, 5.0), (41, 8.0)],
            candidate_attempts: 50_000,
            chase_bits: 12,
            confirmation_reads: 2,
            confirmation_window: 6,
            default_seconds: 60.0,
        },
        Effort::Thorough => EffortProfile {
            variant_limit: 262,
            scales: &[1.0, 2.0, 3.0, 4.0, 6.0],
            channels: &["luma", "green", "red", "blue", "min"],
            adaptive: &[(11, 2.0), (17, 3.0), (25, 5.0), (35, 7.0), (51, 10.0)],
            candidate_attempts: 500_000,
            chase_bits: 18,
            confirmation_reads: 3,
            confirmation_window: 12,
            default_seconds: 600.0,
        },
    }
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecoveryOptionsInput {
    effort: String,
    max_seconds: Option<f64>,
    version: Option<u8>,
    ec_level: Option<String>,
    payload_prefix: Option<String>,
    payload_regex: Option<String>,
    expected_text: Option<String>,
    fallback_encoding: Option<String>,
    batch_index: Option<usize>,
    batch_count: Option<usize>,
}

#[derive(Clone)]
struct RecoveryOptions {
    effort: Effort,
    max_seconds: f64,
    version: Option<u8>,
    ec_level: Option<String>,
    payload_prefix: Option<String>,
    payload_regex: Option<Regex>,
    expected_text: Option<String>,
    fallback_encoding: Option<String>,
    batch_index: usize,
    batch_count: usize,
}

impl RecoveryOptions {
    fn compile(input: RecoveryOptionsInput) -> Result<Self, String> {
        let effort = Effort::parse(&input.effort)?;
        let profile = effort_profile(effort);
        let max_seconds = input.max_seconds.unwrap_or(profile.default_seconds);
        if !max_seconds.is_finite() || max_seconds <= 0.0 {
            return Err("maxSeconds must be positive and finite".into());
        }
        if input
            .version
            .is_some_and(|version| !(1..=40).contains(&version))
        {
            return Err("version must be between 1 and 40".into());
        }
        if input
            .ec_level
            .as_deref()
            .is_some_and(|level| !matches!(level, "L" | "M" | "Q" | "H"))
        {
            return Err("ecLevel must be L, M, Q, or H".into());
        }
        let payload_regex = input
            .payload_regex
            .as_deref()
            .map(Regex::new)
            .transpose()
            .map_err(|error| format!("invalid payloadRegex: {error}"))?;
        let batch_count = input.batch_count.unwrap_or(1);
        let batch_index = input.batch_index.unwrap_or(0);
        if !(1..=8).contains(&batch_count) {
            return Err("batchCount must be between 1 and 8".into());
        }
        if batch_index >= batch_count {
            return Err("batchIndex must be smaller than batchCount".into());
        }
        Ok(Self {
            effort,
            max_seconds,
            version: input.version,
            ec_level: input.ec_level,
            payload_prefix: input.payload_prefix,
            payload_regex,
            expected_text: input.expected_text,
            fallback_encoding: input.fallback_encoding,
            batch_index,
            batch_count,
        })
    }
}

#[derive(Clone, PartialEq)]
enum Terminal {
    Identity,
    Otsu,
    Adaptive {
        window: usize,
        bias: f64,
    },
    GaussianAdaptive {
        window: usize,
        bias: f64,
    },
    Unsharp {
        amount: f64,
    },
    UnsharpAdaptive {
        amount: f64,
        window: usize,
        bias: f64,
    },
    Gamma {
        exponent: f64,
    },
    GammaAdaptive {
        exponent: f64,
        window: usize,
        bias: f64,
    },
    Nearest,
    Invert,
}

#[derive(Clone, PartialEq)]
enum Recipe {
    Original,
    Channel {
        channel: &'static str,
    },
    Calibrated {
        scale: f64,
        terminal: Terminal,
    },
    Standard {
        channel: &'static str,
        scale: f64,
        terminal: Terminal,
    },
}

#[derive(Clone)]
struct VariantSpec {
    name: String,
    quality: f64,
    recipe: Recipe,
}

fn push_variant(plan: &mut Vec<VariantSpec>, limit: usize, spec: VariantSpec) {
    if plan.len() < limit {
        plan.push(spec);
    }
}

fn scale_label(scale: f64) -> usize {
    scale as usize
}

fn push_standard_family(
    plan: &mut Vec<VariantSpec>,
    limit: usize,
    combinations: &[(&'static str, f64)],
    suffix: &str,
    quality: f64,
    terminal: &Terminal,
) {
    for &(channel, scale) in combinations {
        push_variant(
            plan,
            limit,
            VariantSpec {
                name: format!("{channel}-{}x-{suffix}", scale_label(scale)),
                quality: quality + scale * 0.1,
                recipe: Recipe::Standard {
                    channel,
                    scale,
                    terminal: terminal.clone(),
                },
            },
        );
    }
}

fn build_variant_plan_raw(effort: Effort) -> Vec<VariantSpec> {
    let profile = effort_profile(effort);
    let mut plan = Vec::with_capacity(profile.variant_limit);
    push_variant(
        &mut plan,
        profile.variant_limit,
        VariantSpec {
            name: "original".into(),
            quality: 1.0,
            recipe: Recipe::Original,
        },
    );
    push_variant(
        &mut plan,
        profile.variant_limit,
        VariantSpec {
            name: "gray".into(),
            quality: 1.2,
            recipe: Recipe::Channel { channel: "luma" },
        },
    );
    for channel in ["blue", "green", "red"] {
        push_variant(
            &mut plan,
            profile.variant_limit,
            VariantSpec {
                name: format!("channel-{channel}"),
                quality: 1.1,
                recipe: Recipe::Channel { channel },
            },
        );
    }

    let enlarged_scales = profile
        .scales
        .iter()
        .copied()
        .filter(|scale| *scale > 1.0)
        .collect::<Vec<_>>();

    // Keep each scale's calibrated family together: nearby threshold recipes
    // provide fast independent confirmation without rebuilding the resize.
    for &scale in &enlarged_scales {
        let prefix = format!("s{}-lanczos", scale_label(scale));
        for (suffix, quality, terminal) in [
            ("gray", 3.0 + scale * 0.1, Terminal::Identity),
            ("otsu", 3.2 + scale * 0.1, Terminal::Otsu),
            (
                "adapt11-c0",
                4.0 + scale * 0.1,
                Terminal::GaussianAdaptive {
                    window: 11,
                    bias: 0.0,
                },
            ),
            (
                "adapt11-c3",
                3.8 + scale * 0.1,
                Terminal::GaussianAdaptive {
                    window: 11,
                    bias: 3.0,
                },
            ),
            (
                "adapt31-c3",
                3.7 + scale * 0.1,
                Terminal::GaussianAdaptive {
                    window: 31,
                    bias: 3.0,
                },
            ),
        ] {
            push_variant(
                &mut plan,
                profile.variant_limit,
                VariantSpec {
                    name: format!("{prefix}-{suffix}"),
                    quality,
                    recipe: Recipe::Calibrated { scale, terminal },
                },
            );
        }
    }

    let all_combinations = profile
        .scales
        .iter()
        .flat_map(|scale| {
            profile
                .channels
                .iter()
                .map(move |channel| (*channel, *scale))
        })
        .collect::<Vec<_>>();
    let focused_combinations = all_combinations
        .iter()
        .copied()
        .filter(|(channel, scale)| match effort {
            Effort::Fast => *channel == "luma",
            Effort::Balanced => *channel == "luma" || *scale <= 2.0,
            Effort::Thorough => {
                matches!(*channel, "luma" | "min") || matches!(scale_label(*scale), 1 | 2 | 4)
            }
        })
        .collect::<Vec<_>>();
    let reduced_combinations = all_combinations
        .iter()
        .copied()
        .filter(|(channel, scale)| match effort {
            Effort::Fast => *channel == "luma",
            Effort::Balanced => *channel == "luma" || *scale == 1.0,
            Effort::Thorough => matches!(*channel, "luma" | "min"),
        })
        .collect::<Vec<_>>();
    let primary_adaptive = profile
        .adaptive
        .iter()
        .copied()
        .min_by_key(|(window, _)| window.abs_diff(25))
        .expect("every effort profile has an adaptive threshold");

    // Cover every channel/scale with the three highest-yield transforms.
    // More expensive sharpening and threshold diversity use a focused subset.
    push_standard_family(
        &mut plan,
        profile.variant_limit,
        &all_combinations,
        "contrast",
        2.0,
        &Terminal::Identity,
    );
    push_standard_family(
        &mut plan,
        profile.variant_limit,
        &all_combinations,
        "otsu",
        2.4,
        &Terminal::Otsu,
    );
    push_standard_family(
        &mut plan,
        profile.variant_limit,
        &all_combinations,
        &format!(
            "adaptive-{}-{}",
            primary_adaptive.0, primary_adaptive.1 as usize
        ),
        3.0,
        &Terminal::Adaptive {
            window: primary_adaptive.0,
            bias: primary_adaptive.1,
        },
    );
    let amount = if effort == Effort::Thorough {
        1.8
    } else {
        1.35
    };
    let unsharp_suffix = if effort == Effort::Thorough {
        "unsharp-1p8"
    } else {
        "unsharp"
    };
    push_standard_family(
        &mut plan,
        profile.variant_limit,
        &focused_combinations,
        unsharp_suffix,
        2.6,
        &Terminal::Unsharp { amount },
    );
    if effort != Effort::Fast {
        let unsharp_adaptive_suffix = if effort == Effort::Thorough {
            "unsharp-1p8-adaptive"
        } else {
            "unsharp-adaptive"
        };
        push_standard_family(
            &mut plan,
            profile.variant_limit,
            &focused_combinations,
            unsharp_adaptive_suffix,
            3.4,
            &Terminal::UnsharpAdaptive {
                amount,
                window: 25,
                bias: 5.0,
            },
        );
    }
    for &(window, bias) in profile
        .adaptive
        .iter()
        .filter(|value| **value != primary_adaptive)
    {
        push_standard_family(
            &mut plan,
            profile.variant_limit,
            &reduced_combinations,
            &format!("adaptive-{window}-{}", bias as usize),
            3.0,
            &Terminal::Adaptive { window, bias },
        );
    }
    if effort == Effort::Thorough {
        for exponent in [0.7, 1.35] {
            push_standard_family(
                &mut plan,
                profile.variant_limit,
                &focused_combinations,
                &format!("gamma-{exponent}"),
                2.2,
                &Terminal::Gamma { exponent },
            );
            push_standard_family(
                &mut plan,
                profile.variant_limit,
                &reduced_combinations,
                &format!("gamma-{exponent}-adaptive"),
                3.2,
                &Terminal::GammaAdaptive {
                    exponent,
                    window: 35,
                    bias: 7.0,
                },
            );
        }
        push_standard_family(
            &mut plan,
            profile.variant_limit,
            &all_combinations,
            "nearest",
            1.8,
            &Terminal::Nearest,
        );
    }
    if effort == Effort::Thorough {
        push_variant(
            &mut plan,
            profile.variant_limit,
            VariantSpec {
                name: "inverted-luma".into(),
                quality: 1.5,
                recipe: Recipe::Standard {
                    channel: "luma",
                    scale: 1.0,
                    terminal: Terminal::Invert,
                },
            },
        );
    }
    plan
}

fn build_variant_plan(effort: Effort) -> Vec<VariantSpec> {
    if effort != Effort::Thorough {
        return build_variant_plan_raw(effort);
    }
    let profile = effort_profile(effort);
    let mut plan = build_variant_plan_raw(Effort::Balanced);
    let mut names = plan
        .iter()
        .map(|variant| variant.name.clone())
        .collect::<BTreeSet<_>>();
    for variant in build_variant_plan_raw(Effort::Thorough) {
        if plan.len() >= profile.variant_limit {
            break;
        }
        if names.insert(variant.name.clone()) {
            plan.push(variant);
        }
    }
    plan
}

struct PreflightAnalysis {
    achromatic: bool,
    preferred_scale: f64,
    module_pitch: Option<f64>,
    channel_rank: BTreeMap<&'static str, usize>,
}

fn preflight_channel_value(rgba: &[u8], channel: &str) -> u8 {
    match channel {
        "red" => rgba[0],
        "green" => rgba[1],
        "blue" => rgba[2],
        "min" => rgba[0].min(rgba[1]).min(rgba[2]),
        _ => (f64::from(rgba[0]) * 0.299 + f64::from(rgba[1]) * 0.587 + f64::from(rgba[2]) * 0.114)
            .round()
            .clamp(0.0, 255.0) as u8,
    }
}

fn otsu_separation(histogram: &[u32; 256], count: usize) -> f64 {
    if count == 0 {
        return 0.0;
    }
    let total = count as f64;
    let mean = histogram
        .iter()
        .enumerate()
        .map(|(value, frequency)| value as f64 * f64::from(*frequency))
        .sum::<f64>()
        / total;
    let variance = histogram
        .iter()
        .enumerate()
        .map(|(value, frequency)| {
            let delta = value as f64 - mean;
            delta * delta * f64::from(*frequency)
        })
        .sum::<f64>();
    if variance <= f64::EPSILON {
        return 0.0;
    }
    let mut background_weight = 0.0;
    let mut background_sum = 0.0;
    let mut best_between = 0.0f64;
    for (value, frequency) in histogram.iter().enumerate() {
        let frequency = f64::from(*frequency);
        background_weight += frequency;
        background_sum += value as f64 * frequency;
        let foreground_weight = total - background_weight;
        if background_weight <= 0.0 || foreground_weight <= 0.0 {
            continue;
        }
        let background_mean = background_sum / background_weight;
        let foreground_mean = (mean * total - background_sum) / foreground_weight;
        best_between = best_between.max(
            background_weight * foreground_weight * (background_mean - foreground_mean).powi(2),
        );
    }
    (best_between / (total * variance)).clamp(0.0, 1.0)
}

fn estimate_module_pitch(pixels: &[u8], width: u32, height: u32) -> Option<f64> {
    let luma = pixels
        .chunks_exact(4)
        .map(|rgba| preflight_channel_value(rgba, "luma"))
        .collect::<Vec<_>>();
    let source = Luma8LuminanceSource::new(luma, width, height);
    let bitmap = BinaryBitmap::new(HybridBinarizer::new(source));
    let detected = Detector::new(bitmap.get_black_matrix())
        .detect_with_hints(&DecodeHints {
            TryHarder: Some(true),
            ..DecodeHints::default()
        })
        .ok()?;
    let points = detected.getPoints();
    if points.len() < 3 {
        return None;
    }
    let dimension = detected.getBits().getWidth() as f64;
    if dimension <= 7.0 {
        return None;
    }
    let distance = |left: rxing::Point, right: rxing::Point| {
        f64::from((left.x - right.x).hypot(left.y - right.y))
    };
    let horizontal = distance(points[1], points[2]);
    let vertical = distance(points[1], points[0]);
    let pitch = (horizontal + vertical) * 0.5 / (dimension - 7.0);
    pitch
        .is_finite()
        .then_some(pitch)
        .filter(|value| *value > 0.0)
}

fn preferred_scale_for(profile: EffortProfile, pitch: Option<f64>, width: u32, height: u32) -> f64 {
    let fallback_scale = if width.min(height) < 170 {
        3.0
    } else if width.min(height) < 320 {
        2.0
    } else {
        1.0
    };
    profile
        .scales
        .iter()
        .copied()
        .min_by(|left, right| {
            let target = 6.0;
            let left_distance = pitch.map_or((left - fallback_scale).abs(), |value| {
                (value * left - target).abs()
            });
            let right_distance = pitch.map_or((right - fallback_scale).abs(), |value| {
                (value * right - target).abs()
            });
            left_distance.total_cmp(&right_distance)
        })
        .unwrap_or(1.0)
}

fn analyze_preflight(source: &WasmImage, profile: EffortProfile) -> PreflightAnalysis {
    let pixels = source.raw();
    let pixel_count = pixels.len() / 4;
    let stride = pixel_count.div_ceil(65_536).max(1);
    let channels = ["luma", "red", "green", "blue", "min"];
    let mut histograms = [[0u32; 256]; 5];
    let mut samples = 0usize;
    let mut chroma_sum = 0usize;
    let mut colorful_samples = 0usize;
    for pixel in pixels.chunks_exact(4).step_by(stride) {
        let maximum = pixel[0].max(pixel[1]).max(pixel[2]);
        let minimum = pixel[0].min(pixel[1]).min(pixel[2]);
        let chroma = usize::from(maximum - minimum);
        chroma_sum += chroma;
        colorful_samples += usize::from(chroma > 12);
        for (index, channel) in channels.iter().enumerate() {
            let value = preflight_channel_value(pixel, channel);
            histograms[index][usize::from(value)] += 1;
        }
        samples += 1;
    }
    let mean_chroma = chroma_sum as f64 / samples.max(1) as f64;
    let colorful_ratio = colorful_samples as f64 / samples.max(1) as f64;
    let achromatic = mean_chroma <= 6.0 && colorful_ratio <= 0.02;

    let mut scored_channels = channels
        .iter()
        .enumerate()
        .filter(|(_, channel)| **channel == "luma" || profile.channels.contains(channel))
        .map(|(index, channel)| (*channel, otsu_separation(&histograms[index], samples)))
        .collect::<Vec<_>>();
    scored_channels
        .sort_by(|left, right| right.1.total_cmp(&left.1).then_with(|| left.0.cmp(right.0)));
    let channel_rank = scored_channels
        .into_iter()
        .enumerate()
        .map(|(rank, (channel, _))| (channel, rank))
        .collect();

    let pitch = estimate_module_pitch(&pixels, source.width(), source.height());
    let preferred_scale = preferred_scale_for(profile, pitch, source.width(), source.height());
    PreflightAnalysis {
        achromatic,
        preferred_scale,
        module_pitch: pitch,
        channel_rank,
    }
}

fn variant_channel(spec: &VariantSpec) -> Option<&'static str> {
    match &spec.recipe {
        Recipe::Original => None,
        Recipe::Channel { channel } | Recipe::Standard { channel, .. } => Some(channel),
        Recipe::Calibrated { .. } => Some("luma"),
    }
}

fn variant_scale(spec: &VariantSpec) -> f64 {
    match &spec.recipe {
        Recipe::Original | Recipe::Channel { .. } => 1.0,
        Recipe::Calibrated { scale, .. } | Recipe::Standard { scale, .. } => *scale,
    }
}

fn sort_variant_plan(
    plan: &mut [VariantSpec],
    preferred_scale: f64,
    channel_rank: &BTreeMap<&'static str, usize>,
) {
    plan.sort_by(|left, right| {
        let base_rank = |spec: &VariantSpec| match spec.recipe {
            Recipe::Original => 0usize,
            Recipe::Channel { .. } => 1,
            _ => 2,
        };
        let left_base = base_rank(left);
        let right_base = base_rank(right);
        left_base
            .cmp(&right_base)
            .then_with(|| {
                (variant_scale(left) - preferred_scale)
                    .abs()
                    .total_cmp(&(variant_scale(right) - preferred_scale).abs())
            })
            .then_with(|| {
                let recipe_rank = |spec: &VariantSpec| match spec.recipe {
                    Recipe::Calibrated { .. } => 0usize,
                    Recipe::Standard { .. } => 1,
                    _ => 0,
                };
                recipe_rank(left).cmp(&recipe_rank(right))
            })
            .then_with(|| {
                let rank = |spec: &VariantSpec| {
                    variant_channel(spec)
                        .and_then(|channel| channel_rank.get(channel).copied())
                        .unwrap_or(usize::MAX)
                };
                rank(left).cmp(&rank(right))
            })
    });
}

fn prioritize_variant_plan(
    source: &WasmImage,
    effort: Effort,
) -> (Vec<VariantSpec>, PreflightAnalysis) {
    let profile = effort_profile(effort);
    let analysis = analyze_preflight(source, profile);
    let mut plan = build_variant_plan(effort);
    if analysis.achromatic {
        plan.retain(|spec| variant_channel(spec).is_none_or(|channel| channel == "luma"));
    }
    if effort == Effort::Thorough {
        let balanced_names = build_variant_plan(Effort::Balanced)
            .into_iter()
            .map(|variant| variant.name)
            .collect::<BTreeSet<_>>();
        let (mut balanced, mut additional): (Vec<_>, Vec<_>) = plan
            .into_iter()
            .partition(|variant| balanced_names.contains(&variant.name));
        let balanced_scale = preferred_scale_for(
            effort_profile(Effort::Balanced),
            analysis.module_pitch,
            source.width(),
            source.height(),
        );
        sort_variant_plan(&mut balanced, balanced_scale, &analysis.channel_rank);
        sort_variant_plan(
            &mut additional,
            analysis.preferred_scale,
            &analysis.channel_rank,
        );
        balanced.extend(additional);
        plan = balanced;
    } else {
        sort_variant_plan(&mut plan, analysis.preferred_scale, &analysis.channel_rank);
    }
    (plan, analysis)
}

fn select_variant_batch(
    plan: Vec<VariantSpec>,
    batch_index: usize,
    batch_count: usize,
) -> Vec<VariantSpec> {
    if batch_count == 1 {
        return plan;
    }
    plan.into_iter()
        .enumerate()
        .filter_map(|(index, variant)| (index % batch_count == batch_index).then_some(variant))
        .collect()
}

fn apply_terminal(
    image: WasmImage,
    base: &WasmImage,
    scale: f64,
    terminal: &Terminal,
) -> Option<WasmImage> {
    match terminal {
        Terminal::Identity => Some(image),
        Terminal::Otsu => Some(image.photon_threshold(image.otsu_level())),
        Terminal::Adaptive { window, bias } => image.adaptive_threshold(*window, *bias).ok(),
        Terminal::GaussianAdaptive { window, bias } => {
            image.gaussian_adaptive_threshold(*window, *bias).ok()
        }
        Terminal::Unsharp { amount } => image.unsharp(*amount).ok(),
        Terminal::UnsharpAdaptive {
            amount,
            window,
            bias,
        } => image
            .unsharp(*amount)
            .ok()?
            .adaptive_threshold(*window, *bias)
            .ok(),
        Terminal::Gamma { exponent } => image.gamma(*exponent).ok(),
        Terminal::GammaAdaptive {
            exponent,
            window,
            bias,
        } => image
            .gamma(*exponent)
            .ok()?
            .adaptive_threshold(*window, *bias)
            .ok(),
        Terminal::Nearest => base.photon_resize(scale, "nearest").ok(),
        Terminal::Invert => Some(image.photon_invert()),
    }
}

struct CachedImage {
    pixels: Vec<u8>,
    width: u32,
    height: u32,
}

impl CachedImage {
    fn capture(image: &WasmImage) -> Self {
        Self {
            pixels: image.raw(),
            width: image.width(),
            height: image.height(),
        }
    }

    fn image(&self) -> WasmImage {
        WasmImage::from_pixels(self.pixels.clone(), self.width, self.height)
    }
}

#[derive(Default)]
struct RenderCache {
    calibrated: BTreeMap<usize, CachedImage>,
    standard_bases: BTreeMap<&'static str, CachedImage>,
    standard_scaled: BTreeMap<(&'static str, usize), CachedImage>,
}

fn render_variant(
    source: &WasmImage,
    spec: &VariantSpec,
    cache: &mut RenderCache,
) -> Option<WasmImage> {
    match &spec.recipe {
        Recipe::Original => Some(WasmImage::from_pixels(
            source.raw(),
            source.width(),
            source.height(),
        )),
        Recipe::Channel { channel } => source.grayscale(channel).ok(),
        Recipe::Calibrated { scale, terminal } => {
            let key = scale_label(*scale);
            if let Entry::Vacant(entry) = cache.calibrated.entry(key) {
                let gray = source.grayscale("luma").ok()?;
                let scaled = gray.lanczos_resize(*scale, 4).ok()?;
                entry.insert(CachedImage::capture(&scaled));
            }
            let scaled = cache.calibrated.get(&key)?.image();
            apply_terminal(scaled, source, *scale, terminal)
        }
        Recipe::Standard {
            channel,
            scale,
            terminal,
        } => {
            if let Entry::Vacant(entry) = cache.standard_bases.entry(*channel) {
                let gray = source.grayscale(channel).ok()?;
                let contrasted = gray.auto_contrast();
                entry.insert(CachedImage::capture(&contrasted));
            }
            let scale_key = (*channel, scale_label(*scale));
            if let Entry::Vacant(entry) = cache.standard_scaled.entry(scale_key) {
                let base = cache.standard_bases.get(channel)?.image();
                let scaled = if *scale == 1.0 {
                    base
                } else {
                    base.photon_resize(*scale, "lanczos3").ok()?
                };
                entry.insert(CachedImage::capture(&scaled));
            }
            let contrasted = cache.standard_bases.get(channel)?.image();
            let scaled = cache.standard_scaled.get(&scale_key)?.image();
            apply_terminal(scaled, &contrasted, *scale, terminal)
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RecoveryCandidate {
    payload: Vec<u8>,
    text: String,
    version: u8,
    ec_level: String,
    mask: u8,
    matrix: Vec<u8>,
    matrix_kind: String,
    corrected_symbols: usize,
    score: f64,
    score_components: BTreeMap<String, f64>,
    confidence: String,
    source: String,
    evidence_count: usize,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RecoveryDiagnostics {
    examined_variants: usize,
    valid_reads: usize,
    invalid_reads: usize,
    soft_decode_attempts: usize,
    elapsed_seconds: f64,
    input_count: usize,
    runtime: String,
}

#[derive(Clone, Deserialize, Serialize)]
struct PreviewImage {
    width: u32,
    height: u32,
    data: Vec<u8>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RecoveryResult {
    status: String,
    candidates: Vec<RecoveryCandidate>,
    diagnostics: RecoveryDiagnostics,
    discarded_frames: Vec<String>,
    termination_reason: Option<String>,
    best_variant: Option<PreviewImage>,
    best_variant_name: Option<String>,
}

fn ec_level_name(value: u16) -> Option<&'static str> {
    match value {
        0 => Some("M"),
        1 => Some("L"),
        2 => Some("H"),
        3 => Some("Q"),
        _ => None,
    }
}

fn decode_matrix(matrix: &[u8], size: usize) -> Option<(rqrr::MetaData, Vec<u8>)> {
    let simple = SimpleGrid::from_func(size, |x, y| matrix[y * size + x] != 0);
    let grid = Grid::new(simple);
    let mut payload = Vec::new();
    let metadata = grid.decode_to(&mut payload).ok()?;
    Some((metadata, payload))
}

fn hint_score(
    payload: &[u8],
    text: &str,
    version: u8,
    ec_level: &str,
    options: &RecoveryOptions,
) -> f64 {
    let mut score = 0.0;
    if let Some(expected) = options.version {
        score += if expected == version { 3.0 } else { -6.0 };
    }
    if let Some(expected) = options.ec_level.as_deref() {
        score += if expected == ec_level { 3.0 } else { -6.0 };
    }
    if let Some(prefix) = options.payload_prefix.as_deref() {
        score += if text.starts_with(prefix) { 5.0 } else { -5.0 };
    }
    if let Some(expected) = options.expected_text.as_deref() {
        score += if text == expected { 10.0 } else { -8.0 };
    }
    if let Some(pattern) = &options.payload_regex {
        score += if pattern.is_match(text) { 5.0 } else { -5.0 };
    }
    if payload.is_empty() {
        score -= 20.0;
    }
    score
}

struct DecodeOutcome {
    candidates: Vec<RecoveryCandidate>,
    valid_reads: usize,
    invalid_reads: usize,
    attempts: usize,
}

fn bilinear_luma(pixels: &[u8], width: usize, height: usize, x: f32, y: f32) -> f32 {
    let x = x.clamp(0.0, width.saturating_sub(1) as f32);
    let y = y.clamp(0.0, height.saturating_sub(1) as f32);
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = (x0 + 1).min(width.saturating_sub(1));
    let y1 = (y0 + 1).min(height.saturating_sub(1));
    let tx = x - x0 as f32;
    let ty = y - y0 as f32;
    let sample = |sx: usize, sy: usize| f32::from(pixels[(sy * width + sx) * 4]);
    let top = sample(x0, y0) * (1.0 - tx) + sample(x1, y0) * tx;
    let bottom = sample(x0, y1) * (1.0 - tx) + sample(x1, y1) * tx;
    top * (1.0 - ty) + bottom * ty
}

fn classify_module_means(means: Vec<f32>) -> Option<(Vec<u8>, Vec<f32>)> {
    let mut dark = means.iter().copied().fold(255.0f32, f32::min);
    let mut light = means.iter().copied().fold(0.0f32, f32::max);
    if light - dark < 8.0 {
        return None;
    }
    for _ in 0..8 {
        let split = (dark + light) * 0.5;
        let mut dark_sum = 0.0;
        let mut dark_count = 0usize;
        let mut light_sum = 0.0;
        let mut light_count = 0usize;
        for value in &means {
            if *value < split {
                dark_sum += *value;
                dark_count += 1;
            } else {
                light_sum += *value;
                light_count += 1;
            }
        }
        if dark_count == 0 || light_count == 0 {
            break;
        }
        dark = dark_sum / dark_count as f32;
        light = light_sum / light_count as f32;
    }
    let threshold = (dark + light) * 0.5;
    let half_range = ((light - dark) * 0.5).max(1.0);
    let modules = means
        .iter()
        .map(|value| u8::from(*value < threshold))
        .collect();
    let confidence = means
        .iter()
        .map(|value| ((value - threshold).abs() / half_range).clamp(0.01, 1.0))
        .collect();
    Some((modules, confidence))
}

fn resample_with_transform(
    pixels: &[u8],
    width: usize,
    height: usize,
    transform: PerspectiveTransform,
    size: usize,
) -> Option<(Vec<u8>, Vec<f32>)> {
    resample_with_adjustment(
        pixels, width, height, transform, size, 1.0, 1.0, 0.0, 0.0, 0.24,
    )
}

#[allow(clippy::too_many_arguments)]
fn resample_with_adjustment(
    pixels: &[u8],
    width: usize,
    height: usize,
    transform: PerspectiveTransform,
    size: usize,
    scale_x: f32,
    scale_y: f32,
    grid_offset_x: f32,
    grid_offset_y: f32,
    sample_radius: f32,
) -> Option<(Vec<u8>, Vec<f32>)> {
    let offsets = [-sample_radius, 0.0, sample_radius];
    let center = size as f32 * 0.5;
    let mut means = Vec::with_capacity(size * size);
    for module_y_index in 0..size {
        for module_x_index in 0..size {
            let mut sum = 0.0;
            for offset_y in offsets {
                for offset_x in offsets {
                    let module_x = (module_x_index as f32 + 0.5 - center) * scale_x
                        + center
                        + grid_offset_x
                        + offset_x;
                    let module_y = (module_y_index as f32 + 0.5 - center) * scale_y
                        + center
                        + grid_offset_y
                        + offset_y;
                    let image_point = transform.transform_point(point(module_x, module_y));
                    sum += bilinear_luma(pixels, width, height, image_point.x, image_point.y);
                }
            }
            means.push(sum / (offsets.len() * offsets.len()) as f32);
        }
    }
    classify_module_means(means)
}

fn resample_rxing_modules(
    pixels: &[u8],
    width: usize,
    height: usize,
    detected: &impl DetectorRXingResult,
    size: usize,
) -> Option<(Vec<u8>, Vec<f32>)> {
    let transform = rxing_transform(detected, size)?;
    resample_with_transform(pixels, width, height, transform, size)
}

fn rxing_transform(
    detected: &impl DetectorRXingResult,
    size: usize,
) -> Option<PerspectiveTransform> {
    let points = detected.getPoints();
    if points.len() < 3 || size <= 7 {
        return None;
    }
    let bottom_left = points[0];
    let top_left = points[1];
    let top_right = points[2];
    let dimension_edge = size as f32 - 3.5;
    let (source_bottom_right, image_bottom_right) = if points.len() >= 4 {
        (dimension_edge - 3.0, points[3])
    } else {
        (
            dimension_edge,
            point(
                top_right.x - top_left.x + bottom_left.x,
                top_right.y - top_left.y + bottom_left.y,
            ),
        )
    };
    let destination = Quadrilateral::new(
        point(3.5, 3.5),
        point(dimension_edge, 3.5),
        point(source_bottom_right, source_bottom_right),
        point(3.5, dimension_edge),
    );
    let source = Quadrilateral::new(top_left, top_right, image_bottom_right, bottom_left);
    PerspectiveTransform::quadrilateralToQuadrilateral(destination, source).ok()
}

fn format_distance(modules: &[u8], size: usize, ec_level: &str, mask: u8) -> usize {
    let Ok(bits) = format_bits_native(ec_level, mask) else {
        return usize::MAX;
    };
    let mut positions = Vec::with_capacity(30);
    for index in 0..6 {
        positions.push((8, index, index));
    }
    positions.push((8, 7, 6));
    positions.push((8, 8, 7));
    positions.push((7, 8, 8));
    for index in 9..15 {
        positions.push((14 - index, 8, index));
    }
    for index in 0..8 {
        positions.push((size - 1 - index, 8, index));
    }
    for index in 8..15 {
        positions.push((8, size - 15 + index, index));
    }
    positions
        .into_iter()
        .filter(|(x, y, bit)| modules[y * size + x] != ((bits >> bit) & 1) as u8)
        .count()
}

fn ranked_formats(modules: &[u8], size: usize) -> Vec<(&'static str, u8, usize)> {
    let mut formats = Vec::with_capacity(32);
    for ec_level in ["L", "M", "Q", "H"] {
        for mask in 0..8 {
            formats.push((
                ec_level,
                mask,
                format_distance(modules, size, ec_level, mask),
            ));
        }
    }
    formats.sort_by_key(|(_, _, distance)| *distance);
    formats
}

fn decode_variant(
    image: &WasmImage,
    evidence_source: &WasmImage,
    variant_name: &str,
    quality: f64,
    options: &RecoveryOptions,
    max_attempts: usize,
    chase_bits: usize,
) -> DecodeOutcome {
    let pixels = image.raw();
    let mut outcome = DecodeOutcome {
        candidates: Vec::new(),
        valid_reads: 0,
        invalid_reads: 0,
        attempts: 0,
    };

    let width = image.width() as usize;
    let height = image.height() as usize;
    struct ModuleGrid {
        version: u8,
        modules: Vec<u8>,
        confidence: Vec<f32>,
        deformed: bool,
    }
    let mut module_grids = Vec::<ModuleGrid>::new();

    let luma = pixels.chunks_exact(4).map(|rgba| rgba[0]).collect();
    let source = Luma8LuminanceSource::new(luma, image.width(), image.height());
    let bitmap = BinaryBitmap::new(HybridBinarizer::new(source));
    let mut hints = DecodeHints {
        TryHarder: Some(true),
        ..DecodeHints::default()
    };
    if let Some(encoding) = options.fallback_encoding.as_deref() {
        hints.CharacterSet = Some(encoding.into());
    }

    let mut finder_patterns = FindFinderPatterns(bitmap.get_black_matrix(), true, 0);
    for finder_set in GenerateFinderPatternSets(&mut finder_patterns) {
        let Ok(detected) = SampleQR(bitmap.get_black_matrix(), &finder_set) else {
            continue;
        };
        let bits = detected.getBits();
        let size = bits.getWidth() as usize;
        if !(21..=177).contains(&size)
            || size != bits.getHeight() as usize
            || !(size - 17).is_multiple_of(4)
        {
            continue;
        }
        let version = ((size - 17) / 4) as u8;
        if options.version.is_some_and(|hint| hint != version) {
            continue;
        }
        let mut modules = Vec::with_capacity(size * size);
        for y in 0..size {
            for x in 0..size {
                modules.push(u8::from(bits.get(x as u32, y as u32)));
            }
        }
        if !module_grids
            .iter()
            .any(|known| known.version == version && known.modules == modules)
        {
            module_grids.push(ModuleGrid {
                version,
                modules,
                confidence: vec![1.0; size * size],
                deformed: false,
            });
        }
    }

    // The C++-port finder is generally faster in WASM. rqrr remains the
    // independent localization fallback and the verifier for rebuilt matrices.
    if module_grids.is_empty() {
        let mut prepared = PreparedImage::prepare_from_greyscale(width, height, |x, y| {
            pixels[(y * width + x) * 4]
        });
        for grid in prepared.detect_grids() {
            let size = grid.grid.size();
            if !(21..=177).contains(&size) || !(size - 17).is_multiple_of(4) {
                continue;
            }
            let version = ((size - 17) / 4) as u8;
            if options.version.is_some_and(|hint| hint != version) {
                continue;
            }
            let mut detected_modules = Vec::with_capacity(size * size);
            for y in 0..size {
                for x in 0..size {
                    detected_modules.push(u8::from(grid.grid.bit(y, x)));
                }
            }
            module_grids.push(ModuleGrid {
                version,
                modules: detected_modules,
                confidence: vec![1.0; size * size],
                deformed: false,
            });
        }
    }

    let deformed_search = chase_bits > 0 && variant_name.contains("otsu");
    if (module_grids.is_empty() || deformed_search)
        && let Ok(detected) = Detector::new(bitmap.get_black_matrix()).detect_with_hints(&hints)
    {
        let evidence = evidence_source.grayscale("luma").ok().and_then(|gray| {
            if gray.width() == image.width() && gray.height() == image.height() {
                Some(gray)
            } else {
                gray.lanczos_resize(image.width() as f64 / gray.width() as f64, 4)
                    .ok()
            }
        });
        let evidence_pixels = evidence
            .as_ref()
            .filter(|value| value.width() == image.width() && value.height() == image.height())
            .map(WasmImage::raw);
        let evidence_pixels = evidence_pixels.as_deref().unwrap_or(&pixels);
        let detected_size = detected.getBits().getWidth() as i32;
        let detected_version = ((detected_size - 17) / 4).clamp(1, 40) as u8;
        let versions: Vec<u8> = if let Some(version) = options.version {
            vec![version]
        } else {
            let mut versions = Vec::new();
            for offset in [0i16, 1, -1, 2, -2, 3, -3, 4, -4] {
                let version = i16::from(detected_version) + offset;
                if (1..=40).contains(&version) {
                    versions.push(version as u8);
                }
            }
            versions
        };
        for version in versions {
            let size = usize::from(version) * 4 + 17;
            if let Some((modules, confidence)) =
                resample_rxing_modules(evidence_pixels, width, height, &detected, size)
            {
                module_grids.push(ModuleGrid {
                    version,
                    modules,
                    confidence,
                    deformed: false,
                });
            }
        }
        if deformed_search {
            let size = usize::from(detected_version) * 4 + 17;
            if options.version.is_none_or(|hint| hint == detected_version)
                && let Some(transform) = rxing_transform(&detected, size)
            {
                for scale_x in [0.91f32, 0.93, 0.95] {
                    for scale_y in [1.0f32, 1.02] {
                        for grid_offset_x in [0.1f32, 0.2, 0.3, 0.4] {
                            for grid_offset_y in [-0.3f32, -0.2] {
                                let Some((modules, confidence)) = resample_with_adjustment(
                                    evidence_pixels,
                                    width,
                                    height,
                                    transform,
                                    size,
                                    scale_x,
                                    scale_y,
                                    grid_offset_x,
                                    grid_offset_y,
                                    0.0,
                                ) else {
                                    continue;
                                };
                                module_grids.push(ModuleGrid {
                                    version: detected_version,
                                    modules,
                                    confidence,
                                    deformed: true,
                                });
                            }
                        }
                    }
                }
            }
        }
    }
    let deformed_grids = module_grids
        .iter()
        .filter(|grid| grid.deformed)
        .collect::<Vec<_>>();
    if deformed_grids.len() >= 3 {
        let version = deformed_grids[0].version;
        let module_count = deformed_grids[0].modules.len();
        if deformed_grids
            .iter()
            .all(|grid| grid.version == version && grid.modules.len() == module_count)
        {
            let mut modules = Vec::with_capacity(module_count);
            let mut confidence = Vec::with_capacity(module_count);
            for index in 0..module_count {
                let (dark_weight, total_weight) =
                    deformed_grids
                        .iter()
                        .fold((0.0f32, 0.0f32), |(dark, total), grid| {
                            let weight = grid.confidence[index].max(0.05);
                            (
                                dark + if grid.modules[index] == 1 {
                                    weight
                                } else {
                                    0.0
                                },
                                total + weight,
                            )
                        });
                modules.push(u8::from(dark_weight * 2.0 >= total_weight));
                confidence.push(
                    ((dark_weight * 2.0 - total_weight).abs() / total_weight.max(0.01))
                        .clamp(0.01, 1.0),
                );
            }
            module_grids.push(ModuleGrid {
                version,
                modules,
                confidence,
                deformed: true,
            });
        }
    }
    if module_grids.is_empty() {
        return outcome;
    }

    struct Hypothesis {
        structure_error: f32,
        format_distance: usize,
        version: u8,
        ec_level: &'static str,
        mask: u8,
        modules: Vec<u8>,
        confidence: Vec<f32>,
        deformed: bool,
    }
    let mut hypotheses = Vec::new();
    for grid in module_grids {
        let ModuleGrid {
            version,
            modules,
            confidence,
            deformed,
        } = grid;
        let size = usize::from(version) * 4 + 17;
        let formats = ranked_formats(&modules, size);
        let best_format_distance = formats[0].2;
        for (ec_level, mask, format_distance) in formats
            .into_iter()
            .filter(|(ec_level, _, distance)| {
                options
                    .ec_level
                    .as_deref()
                    .is_none_or(|hint| hint == *ec_level)
                    && ((deformed && *distance <= 8)
                        || (!deformed && *distance <= best_format_distance + 4))
            })
            .take(if deformed { 32 } else { 6 })
        {
            let Ok((expected, functions)) = function_matrix_native(version, ec_level, mask) else {
                continue;
            };
            let mut error = 0.0f32;
            let mut weight = 0.0f32;
            for index in 0..modules.len() {
                if functions[index] {
                    let reliability = confidence[index].max(0.05);
                    weight += reliability;
                    if modules[index] != expected[index] {
                        error += reliability;
                    }
                }
            }
            hypotheses.push(Hypothesis {
                structure_error: error / weight.max(1.0),
                format_distance,
                version,
                ec_level,
                mask,
                modules: modules.clone(),
                confidence: confidence.clone(),
                deformed,
            });
        }
    }
    hypotheses.sort_by(|left, right| {
        left.structure_error
            .total_cmp(&right.structure_error)
            .then_with(|| left.format_distance.cmp(&right.format_distance))
    });

    // A curved surface can leave a complete QR segment intact while moving
    // padding/ECC modules too far from a single projective grid. Preserve the
    // observed segment bits, regenerate only the deterministic tail, and keep
    // the result only when the clean matrix independently decodes and still
    // agrees strongly with the sampled image.
    for hypothesis in hypotheses.iter().filter(|value| value.deformed) {
        let size = usize::from(hypothesis.version) * 4 + 17;
        let Ok(extracted) = extract_codewords_native(
            &hypothesis.modules,
            &hypothesis.confidence,
            hypothesis.version,
            hypothesis.ec_level,
            hypothesis.mask,
        ) else {
            continue;
        };
        outcome.attempts += 1;
        let Ok(Some(stream)) = repair_data_tail_native(
            &extracted.codewords,
            hypothesis.version,
            hypothesis.ec_level,
        ) else {
            continue;
        };
        let Ok(matrix) = matrix_from_codewords_native(
            &stream.codewords,
            hypothesis.version,
            hypothesis.ec_level,
            hypothesis.mask,
        ) else {
            continue;
        };
        let (matched_weight, total_weight) = matrix
            .iter()
            .zip(&hypothesis.modules)
            .zip(&hypothesis.confidence)
            .fold(
                (0.0f32, 0.0f32),
                |(matched, total), ((expected, observed), weight)| {
                    (
                        matched + if expected == observed { *weight } else { 0.0 },
                        total + *weight,
                    )
                },
            );
        let image_agreement = matched_weight / total_weight.max(1.0);
        if image_agreement < 0.82 {
            continue;
        }
        let Some((verified_meta, payload)) = decode_matrix(&matrix, size) else {
            continue;
        };
        if verified_meta.version.0 as u8 != hypothesis.version
            || ec_level_name(verified_meta.ecc_level) != Some(hypothesis.ec_level)
            || verified_meta.mask as u8 != hypothesis.mask
        {
            continue;
        }
        let text = String::from_utf8_lossy(&payload).into_owned();
        let payload_hints = hint_score(
            &payload,
            &text,
            hypothesis.version,
            hypothesis.ec_level,
            options,
        );
        let mut score_components = BTreeMap::new();
        score_components.insert("decoder".into(), 100.0);
        score_components.insert("imageVariant".into(), quality);
        score_components.insert("payloadHints".into(), payload_hints);
        score_components.insert(
            "utf8Evidence".into(),
            if std::str::from_utf8(&payload).is_ok() {
                4.0
            } else {
                -4.0
            },
        );
        score_components.insert(
            "formatEvidence".into(),
            -(hypothesis.format_distance as f64),
        );
        score_components.insert(
            "structureEvidence".into(),
            -f64::from(hypothesis.structure_error * 20.0),
        );
        score_components.insert("imageAgreement".into(), f64::from(image_agreement * 20.0));
        score_components.insert("correctionCost".into(), -f64::from(stream.search_cost));
        let score = score_components.values().sum();
        outcome.candidates.push(RecoveryCandidate {
            payload,
            text,
            version: hypothesis.version,
            ec_level: hypothesis.ec_level.into(),
            mask: hypothesis.mask,
            matrix,
            matrix_kind: "exact".into(),
            corrected_symbols: stream.corrected_symbols,
            score,
            score_components,
            confidence: "high".into(),
            source: format!("rxing-rust:{variant_name}:deformed-v{}", hypothesis.version),
            evidence_count: 1,
        });
    }

    let skip_standard = !outcome.candidates.is_empty();
    for hypothesis in hypotheses
        .into_iter()
        .filter(|value| !value.deformed && !skip_standard)
        .take(8)
    {
        let Hypothesis {
            structure_error,
            format_distance,
            version,
            ec_level,
            mask,
            modules,
            confidence,
            deformed: _,
        } = hypothesis;
        let size = usize::from(version) * 4 + 17;
        let extracted =
            match extract_codewords_native(&modules, &confidence, version, ec_level, mask) {
                Ok(extracted) => extracted,
                Err(_) => continue,
            };
        let (streams, attempts) = match correct_interleaved_native(
            &extracted.codewords,
            &extracted.bit_reliability,
            version,
            ec_level,
            chase_bits,
            max_attempts.min(2_000),
            8,
        ) {
            Ok(value) => value,
            Err(_) => continue,
        };
        outcome.attempts += attempts;
        for stream in streams {
            let matrix =
                match matrix_from_codewords_native(&stream.codewords, version, ec_level, mask) {
                    Ok(matrix) => matrix,
                    Err(_) => continue,
                };
            let Some((verified_meta, verified_payload)) = decode_matrix(&matrix, size) else {
                continue;
            };
            if verified_meta.version.0 as u8 != version
                || ec_level_name(verified_meta.ecc_level) != Some(ec_level)
                || verified_meta.mask as u8 != mask
            {
                continue;
            }
            let payload = verified_payload;
            let text = String::from_utf8_lossy(&payload).into_owned();
            let payload_hints = hint_score(&payload, &text, version, ec_level, options);
            let mut score_components = BTreeMap::new();
            score_components.insert("decoder".into(), 100.0);
            score_components.insert("imageVariant".into(), quality);
            score_components.insert("payloadHints".into(), payload_hints);
            score_components.insert("formatEvidence".into(), -(format_distance as f64));
            score_components.insert(
                "structureEvidence".into(),
                -f64::from(structure_error * 20.0),
            );
            score_components.insert("correctionCost".into(), -f64::from(stream.search_cost));
            let score = score_components.values().sum();
            outcome.candidates.push(RecoveryCandidate {
                payload,
                text,
                version,
                ec_level: ec_level.into(),
                mask,
                matrix,
                matrix_kind: "exact".into(),
                corrected_symbols: stream.corrected_symbols,
                score,
                score_components,
                confidence: "high".into(),
                source: format!("rxing-rust:{variant_name}:resampled-v{version}"),
                evidence_count: 1,
            });
        }
    }
    let mut unique_candidates = BTreeMap::<Vec<u8>, RecoveryCandidate>::new();
    for candidate in outcome.candidates.drain(..) {
        match unique_candidates.entry(candidate.payload.clone()) {
            Entry::Vacant(entry) => {
                entry.insert(candidate);
            }
            Entry::Occupied(mut entry) => {
                let existing = entry.get_mut();
                let evidence_count = existing.evidence_count + candidate.evidence_count;
                if candidate.score > existing.score {
                    *existing = candidate;
                }
                existing.evidence_count = evidence_count;
                if existing.source.contains(":deformed-v") {
                    existing.score_components.insert(
                        "deformationGeometry".into(),
                        evidence_count.saturating_sub(1) as f64 * 2.0,
                    );
                    existing.score = existing.score_components.values().sum();
                }
            }
        }
    }
    outcome.candidates = unique_candidates.into_values().collect();
    if outcome.candidates.is_empty() {
        outcome.invalid_reads = 1;
    } else {
        outcome.valid_reads = 1;
    }
    outcome
}

fn merge_candidate(
    candidates: &mut BTreeMap<Vec<u8>, RecoveryCandidate>,
    candidate: RecoveryCandidate,
) {
    if let Some(existing) = candidates.get_mut(&candidate.payload) {
        existing.evidence_count += 1;
        existing.score_components.insert(
            "independentEvidence".into(),
            existing.evidence_count as f64 * 4.0,
        );
        existing.score = existing.score.max(candidate.score) + 4.0;
    } else {
        candidates.insert(candidate.payload.clone(), candidate);
    }
}

fn fused_variants(observations: &[WasmImage], effort: Effort) -> Vec<VariantSpecImage> {
    let Some(first) = observations.first() else {
        return Vec::new();
    };
    if observations.len() < 2
        || observations
            .iter()
            .any(|image| image.width() != first.width() || image.height() != first.height())
    {
        return Vec::new();
    }
    let mut processed = Vec::with_capacity(observations.len());
    for source in observations {
        let Some(gray) = source.grayscale("luma").ok() else {
            return Vec::new();
        };
        processed.push(gray.auto_contrast().raw());
    }
    let pixel_count = first.width() as usize * first.height() as usize;
    let mut fused = vec![0u8; pixel_count * 4];
    for pixel in 0..pixel_count {
        let sum = processed
            .iter()
            .map(|frame| usize::from(frame[pixel * 4]))
            .sum::<usize>();
        let value = (sum as f64 / processed.len() as f64).round() as u8;
        fused[pixel * 4..pixel * 4 + 4].copy_from_slice(&[value, value, value, 255]);
    }
    let fused_image = WasmImage::from_pixels(fused, first.width(), first.height());
    let adaptive_window = if effort == Effort::Thorough { 35 } else { 25 };
    let mut variants = vec![VariantSpecImage {
        name: "fusion-mean".into(),
        quality: 4.0,
        image: WasmImage::from_pixels(fused_image.raw(), fused_image.width(), fused_image.height()),
    }];
    if let Ok(adaptive) = fused_image.adaptive_threshold(adaptive_window, 5.0) {
        variants.push(VariantSpecImage {
            name: "fusion-mean-adaptive".into(),
            quality: 4.5,
            image: adaptive,
        });
    }
    variants
}

struct VariantSpecImage {
    name: String,
    quality: f64,
    image: WasmImage,
}

fn recover_native<F>(
    observations: &[WasmImage],
    options: &RecoveryOptions,
    mut progress: F,
) -> Result<RecoveryResult, String>
where
    F: FnMut(&str, usize, usize, &str),
{
    if observations.is_empty() {
        return Err("at least one observation is required".into());
    }
    let started = clock_seconds();
    let profile = effort_profile(options.effort);
    let balanced_variant_names = if options.effort == Effort::Thorough {
        build_variant_plan(Effort::Balanced)
            .into_iter()
            .map(|variant| variant.name)
            .collect::<BTreeSet<_>>()
    } else {
        BTreeSet::new()
    };
    let observation_plans = observations
        .iter()
        .map(|source| {
            let (plan, analysis) = prioritize_variant_plan(source, options.effort);
            let plan = select_variant_batch(plan, options.batch_index, options.batch_count);
            (plan, analysis)
        })
        .collect::<Vec<_>>();
    let total = observation_plans
        .iter()
        .map(|(plan, _)| plan.len())
        .sum::<usize>()
        + usize::from(observations.len() > 1) * 2;
    let mut diagnostics = RecoveryDiagnostics {
        examined_variants: 0,
        valid_reads: 0,
        invalid_reads: 0,
        soft_decode_attempts: 0,
        elapsed_seconds: 0.0,
        input_count: observations.len(),
        runtime: "rust-wasm+photon+rxing+rqrr".into(),
    };
    let mut candidates = BTreeMap::<Vec<u8>, RecoveryCandidate>::new();
    let mut per_input_payloads = BTreeMap::<usize, BTreeSet<Vec<u8>>>::new();
    let mut variant_hits = BTreeMap::<Vec<u8>, usize>::new();
    let mut variant_previews = BTreeMap::<Vec<u8>, (f64, PreviewImage, String)>::new();
    let mut first_verified_at = None;
    let mut termination_reason = None;

    'observations: for (observation_index, (source, (plan, preflight))) in
        observations.iter().zip(&observation_plans).enumerate()
    {
        progress(
            "preflight",
            diagnostics.examined_variants,
            total,
            &format!(
                "Frame {} · {} · preferred {}×",
                observation_index + 1,
                if preflight.achromatic {
                    "achromatic/luma-only"
                } else {
                    "color/channel-ranked"
                },
                preflight.preferred_scale
            ),
        );
        let mut render_cache = RenderCache::default();
        for spec in plan {
            if clock_seconds() - started >= options.max_seconds {
                termination_reason = Some("time_limit".into());
                break 'observations;
            }
            let confirming_deformed_candidate = candidates
                .values()
                .any(|candidate| candidate.source.contains(":deformed-v"));
            if confirming_deformed_candidate && !spec.name.contains("otsu") {
                continue;
            }
            progress(
                "searching",
                diagnostics.examined_variants,
                total,
                &format!("Frame {} · {}", observation_index + 1, spec.name),
            );
            let Some(image) = render_variant(source, spec, &mut render_cache) else {
                continue;
            };
            diagnostics.examined_variants += 1;
            let decode_profile = if balanced_variant_names.contains(&spec.name) {
                effort_profile(Effort::Balanced)
            } else {
                profile
            };
            let outcome = decode_variant(
                &image,
                source,
                &spec.name,
                spec.quality,
                options,
                decode_profile.candidate_attempts,
                decode_profile.chase_bits,
            );
            diagnostics.valid_reads += outcome.valid_reads;
            diagnostics.invalid_reads += outcome.invalid_reads;
            diagnostics.soft_decode_attempts += outcome.attempts;
            if !outcome.candidates.is_empty() {
                let preview = PreviewImage {
                    width: image.width(),
                    height: image.height(),
                    data: image.raw(),
                };
                for candidate in &outcome.candidates {
                    match variant_previews.entry(candidate.payload.clone()) {
                        Entry::Occupied(mut entry) if candidate.score > entry.get().0 => {
                            entry.insert((candidate.score, preview.clone(), spec.name.clone()));
                        }
                        Entry::Vacant(entry) => {
                            entry.insert((candidate.score, preview.clone(), spec.name.clone()));
                        }
                        Entry::Occupied(_) => {}
                    }
                }
            }
            let variant_payloads = outcome
                .candidates
                .iter()
                .map(|candidate| candidate.payload.clone())
                .collect::<BTreeSet<_>>();
            for candidate in outcome.candidates {
                per_input_payloads
                    .entry(observation_index)
                    .or_default()
                    .insert(candidate.payload.clone());
                merge_candidate(&mut candidates, candidate);
            }
            for payload in variant_payloads {
                *variant_hits.entry(payload).or_default() += 1;
            }
            // Parallel batch workers are speculative. Once one variant yields
            // verified candidates, return that partial immediately so the
            // browser coordinator can accept a decoded result and cancel the
            // remaining workers. A locally ambiguous result is not accepted by
            // the coordinator; the other batches continue and are merged.
            if options.batch_count > 1 && !candidates.is_empty() {
                termination_reason = Some("confidence_limit".into());
                break 'observations;
            }
            if options.batch_count == 1 && observations.len() == 1 && !candidates.is_empty() {
                first_verified_at.get_or_insert(diagnostics.examined_variants);
                if candidates.len() == 1 {
                    let payload = candidates.keys().next().expect("one candidate exists");
                    let enough_confirmations = variant_hits
                        .get(payload)
                        .is_some_and(|hits| *hits >= profile.confirmation_reads);
                    let enough_search_after_first = diagnostics.examined_variants
                        >= first_verified_at.expect("verified candidate was recorded")
                            + profile.confirmation_window;
                    if enough_confirmations && enough_search_after_first {
                        termination_reason = Some("confidence_limit".into());
                        break 'observations;
                    }
                }
            }
        }
    }

    if candidates.is_empty()
        && observations.len() > 1
        && clock_seconds() - started < options.max_seconds
    {
        for variant in fused_variants(observations, options.effort) {
            progress(
                "fusion",
                diagnostics.examined_variants,
                total,
                &variant.name,
            );
            diagnostics.examined_variants += 1;
            let outcome = decode_variant(
                &variant.image,
                &variant.image,
                &variant.name,
                variant.quality,
                options,
                profile.candidate_attempts,
                profile.chase_bits,
            );
            diagnostics.valid_reads += outcome.valid_reads;
            diagnostics.invalid_reads += outcome.invalid_reads;
            diagnostics.soft_decode_attempts += outcome.attempts;
            if !outcome.candidates.is_empty() {
                let preview = PreviewImage {
                    width: variant.image.width(),
                    height: variant.image.height(),
                    data: variant.image.raw(),
                };
                for candidate in &outcome.candidates {
                    match variant_previews.entry(candidate.payload.clone()) {
                        Entry::Occupied(mut entry) if candidate.score > entry.get().0 => {
                            entry.insert((candidate.score, preview.clone(), variant.name.clone()));
                        }
                        Entry::Vacant(entry) => {
                            entry.insert((candidate.score, preview.clone(), variant.name.clone()));
                        }
                        Entry::Occupied(_) => {}
                    }
                }
            }
            for mut candidate in outcome.candidates {
                candidate.evidence_count = observations.len();
                candidate.score_components.insert(
                    "independentEvidence".into(),
                    observations.len() as f64 * 4.0,
                );
                candidate.score += observations.len() as f64 * 4.0;
                merge_candidate(&mut candidates, candidate);
            }
        }
    }

    let mut ordered = candidates.into_values().collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.payload.cmp(&right.payload))
    });
    let mut frame_payload_sets = per_input_payloads
        .values()
        .filter(|payloads| !payloads.is_empty());
    let conflicting_frames = frame_payload_sets.next().is_some_and(|first| {
        let mut common = first.clone();
        let mut compared = false;
        for payloads in frame_payload_sets {
            compared = true;
            common.retain(|payload| payloads.contains(payload));
        }
        compared && common.is_empty()
    });
    let calibrated_deformed_winner = ordered.len() > 1
        && ordered[0].source.contains(":deformed-v")
        && ordered[0].evidence_count >= 4
        && ordered[0].evidence_count >= ordered[1].evidence_count + 2
        && ordered[0].score - ordered[1].score >= 10.0;
    let status = if ordered.is_empty() {
        "unrecoverable"
    } else if conflicting_frames {
        "ambiguous"
    } else if ordered.len() == 1
        || ordered[0].score - ordered[1].score >= 12.0
        || calibrated_deformed_winner
    {
        "decoded"
    } else {
        "ambiguous"
    };
    if status == "ambiguous" {
        for candidate in &mut ordered {
            candidate.confidence = "low".into();
        }
    }
    let (best_variant, best_variant_name) = ordered
        .first()
        .and_then(|candidate| variant_previews.remove(&candidate.payload))
        .map_or((None, None), |(_, preview, name)| {
            (Some(preview), Some(name))
        });
    ordered.truncate(10);
    diagnostics.elapsed_seconds = clock_seconds() - started;
    Ok(RecoveryResult {
        status: status.into(),
        candidates: ordered,
        diagnostics,
        discarded_frames: Vec::new(),
        termination_reason,
        best_variant,
        best_variant_name,
    })
}

fn merge_recovery_results_native(partials: Vec<RecoveryResult>) -> Result<RecoveryResult, String> {
    let Some(first) = partials.first() else {
        return Err("at least one batch result is required".into());
    };
    if partials
        .iter()
        .any(|result| result.diagnostics.input_count != first.diagnostics.input_count)
    {
        return Err("batch results must describe the same number of inputs".into());
    }

    let mut candidates = BTreeMap::<Vec<u8>, RecoveryCandidate>::new();
    let mut previews = BTreeMap::<Vec<u8>, (f64, PreviewImage, String)>::new();
    let mut diagnostics = RecoveryDiagnostics {
        examined_variants: 0,
        valid_reads: 0,
        invalid_reads: 0,
        soft_decode_attempts: 0,
        elapsed_seconds: 0.0,
        input_count: first.diagnostics.input_count,
        runtime: "rust-wasm+photon+rxing+rqrr".into(),
    };
    let mut discarded_frames = BTreeSet::new();
    let mut time_limited = false;

    for mut partial in partials {
        diagnostics.examined_variants += partial.diagnostics.examined_variants;
        diagnostics.valid_reads += partial.diagnostics.valid_reads;
        diagnostics.invalid_reads += partial.diagnostics.invalid_reads;
        diagnostics.soft_decode_attempts += partial.diagnostics.soft_decode_attempts;
        diagnostics.elapsed_seconds = diagnostics
            .elapsed_seconds
            .max(partial.diagnostics.elapsed_seconds);
        time_limited |= partial.termination_reason.as_deref() == Some("time_limit");
        discarded_frames.extend(partial.discarded_frames);

        if let (Some(candidate), Some(preview), Some(name)) = (
            partial.candidates.first(),
            partial.best_variant.take(),
            partial.best_variant_name.take(),
        ) {
            match previews.entry(candidate.payload.clone()) {
                Entry::Occupied(mut entry) if candidate.score > entry.get().0 => {
                    entry.insert((candidate.score, preview, name));
                }
                Entry::Vacant(entry) => {
                    entry.insert((candidate.score, preview, name));
                }
                Entry::Occupied(_) => {}
            }
        }

        for candidate in partial.candidates {
            match candidates.entry(candidate.payload.clone()) {
                Entry::Vacant(entry) => {
                    entry.insert(candidate);
                }
                Entry::Occupied(mut entry) => {
                    let existing = entry.get_mut();
                    let evidence_count = existing.evidence_count + candidate.evidence_count;
                    let independent_score = |value: &RecoveryCandidate| {
                        value.score
                            - value
                                .score_components
                                .get("independentEvidence")
                                .copied()
                                .unwrap_or(0.0)
                    };
                    if independent_score(&candidate) > independent_score(existing) {
                        *existing = candidate;
                    }
                    existing.evidence_count = evidence_count;
                    existing.score_components.insert(
                        "independentEvidence".into(),
                        evidence_count.saturating_sub(1) as f64 * 4.0,
                    );
                    existing.score = existing.score_components.values().sum();
                }
            }
        }
    }

    let mut ordered = candidates.into_values().collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.payload.cmp(&right.payload))
    });
    let calibrated_deformed_winner = ordered.len() > 1
        && ordered[0].source.contains(":deformed-v")
        && ordered[0].evidence_count >= 4
        && ordered[0].evidence_count >= ordered[1].evidence_count + 2
        && ordered[0].score - ordered[1].score >= 10.0;
    let status = if ordered.is_empty() {
        "unrecoverable"
    } else if ordered.len() == 1
        || ordered[0].score - ordered[1].score >= 12.0
        || calibrated_deformed_winner
    {
        "decoded"
    } else {
        "ambiguous"
    };
    if status == "ambiguous" {
        for candidate in &mut ordered {
            candidate.confidence = "low".into();
        }
    }
    let (best_variant, best_variant_name) = ordered
        .first()
        .and_then(|candidate| previews.remove(&candidate.payload))
        .map_or((None, None), |(_, preview, name)| {
            (Some(preview), Some(name))
        });
    ordered.truncate(10);

    Ok(RecoveryResult {
        status: status.into(),
        candidates: ordered,
        diagnostics,
        discarded_frames: discarded_frames.into_iter().collect(),
        termination_reason: time_limited.then(|| "time_limit".into()),
        best_variant,
        best_variant_name,
    })
}

#[wasm_bindgen]
pub fn merge_recovery_results(results: JsValue) -> Result<JsValue, JsValue> {
    let partials = serde_wasm_bindgen::from_value(results)
        .map_err(|error| JsValue::from_str(&format!("invalid batch results: {error}")))?;
    let result =
        merge_recovery_results_native(partials).map_err(|error| JsValue::from_str(&error))?;
    result
        .serialize(&serde_wasm_bindgen::Serializer::new().serialize_maps_as_objects(true))
        .map_err(|error| JsValue::from_str(&error.to_string()))
}

#[wasm_bindgen]
pub struct RecoveryEngine {
    observations: Vec<WasmImage>,
    options: RecoveryOptions,
}

#[wasm_bindgen]
impl RecoveryEngine {
    #[wasm_bindgen(constructor)]
    pub fn new(options: JsValue) -> Result<RecoveryEngine, JsValue> {
        let input: RecoveryOptionsInput = serde_wasm_bindgen::from_value(options)
            .map_err(|error| JsValue::from_str(&format!("invalid recovery options: {error}")))?;
        let options = RecoveryOptions::compile(input).map_err(|error| JsValue::from_str(&error))?;
        Ok(Self {
            observations: Vec::new(),
            options,
        })
    }

    pub fn add_observation(
        &mut self,
        pixels: &[u8],
        width: u32,
        height: u32,
    ) -> Result<(), JsValue> {
        self.observations
            .push(WasmImage::new(pixels, width, height)?);
        Ok(())
    }

    pub fn recover(&self, progress: &Function) -> Result<JsValue, JsValue> {
        let result = recover_native(
            &self.observations,
            &self.options,
            |stage, completed, total, detail| {
                let _ = progress.call4(
                    &JsValue::NULL,
                    &JsValue::from_str(stage),
                    &JsValue::from_f64(completed as f64),
                    &JsValue::from_f64(total as f64),
                    &JsValue::from_str(detail),
                );
            },
        )
        .map_err(|error| JsValue::from_str(&error))?;
        result
            .serialize(&serde_wasm_bindgen::Serializer::new().serialize_maps_as_objects(true))
            .map_err(|error| JsValue::from_str(&error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photon_rs::PhotonImage;

    #[test]
    fn effort_plans_are_bounded_and_keep_the_calibrated_winner() {
        assert_eq!(build_variant_plan(Effort::Fast).len(), 20);
        assert_eq!(build_variant_plan(Effort::Balanced).len(), 102);
        assert_eq!(build_variant_plan(Effort::Thorough).len(), 262);
        for effort in [Effort::Fast, Effort::Balanced, Effort::Thorough] {
            assert!(
                build_variant_plan(effort)
                    .iter()
                    .any(|variant| variant.name == "s2-lanczos-gray")
            );
        }
        let thorough = build_variant_plan(Effort::Thorough);
        for balanced in build_variant_plan(Effort::Balanced) {
            assert!(thorough.iter().any(|variant| {
                variant.name == balanced.name && variant.recipe == balanced.recipe
            }));
        }
    }

    #[test]
    fn preflight_prunes_redundant_channels_for_achromatic_images() {
        let mut pixels = Vec::with_capacity(192 * 192 * 4);
        for row in 0..192 {
            for column in 0..192 {
                let value = if (row / 8 + column / 8) % 2 == 0 {
                    0
                } else {
                    255
                };
                pixels.extend_from_slice(&[value, value, value, 255]);
            }
        }
        let observation = WasmImage::from_pixels(pixels, 192, 192);
        for (effort, expected_variants) in [
            (Effort::Fast, 17),
            (Effort::Balanced, 45),
            (Effort::Thorough, 99),
        ] {
            let (plan, analysis) = prioritize_variant_plan(&observation, effort);
            assert!(analysis.achromatic);
            assert_eq!(plan.len(), expected_variants);
            assert!(plan.iter().all(|variant| {
                variant_channel(variant).is_none_or(|channel| channel == "luma")
            }));
        }
    }

    #[test]
    fn thorough_preflight_runs_the_balanced_phase_first() {
        let pixels = [24u8, 24, 24, 255].repeat(192 * 192);
        let observation = WasmImage::from_pixels(pixels, 192, 192);
        let (balanced, _) = prioritize_variant_plan(&observation, Effort::Balanced);
        let (thorough, _) = prioritize_variant_plan(&observation, Effort::Thorough);
        assert!(thorough.len() > balanced.len());
        assert!(balanced.iter().zip(&thorough).all(|(expected, actual)| {
            expected.name == actual.name && expected.recipe == actual.recipe
        }));
    }

    #[test]
    fn preflight_keeps_color_channels_when_they_carry_distinct_information() {
        let mut pixels = Vec::with_capacity(192 * 192 * 4);
        for row in 0..192 {
            for column in 0..192 {
                let red = if (row / 8 + column / 8) % 2 == 0 {
                    0
                } else {
                    255
                };
                pixels.extend_from_slice(&[red, 128, 128, 255]);
            }
        }
        let observation = WasmImage::from_pixels(pixels, 192, 192);
        let (plan, analysis) = prioritize_variant_plan(&observation, Effort::Balanced);
        assert!(!analysis.achromatic);
        assert_eq!(plan.len(), 102);
        assert!(
            plan.iter()
                .any(|variant| variant_channel(variant) == Some("red"))
        );
        assert!(
            plan.iter()
                .any(|variant| variant_channel(variant) == Some("green"))
        );
        assert!(
            plan.iter()
                .any(|variant| variant_channel(variant) == Some("blue"))
        );
    }

    #[test]
    fn parallel_batches_are_disjoint_and_cover_the_prioritized_plan() {
        let pixels = [24u8, 24, 24, 255].repeat(192 * 192);
        let observation = WasmImage::from_pixels(pixels, 192, 192);
        let (plan, _) = prioritize_variant_plan(&observation, Effort::Balanced);
        let expected = plan
            .iter()
            .map(|variant| variant.name.clone())
            .collect::<BTreeSet<_>>();
        let mut observed = BTreeSet::new();
        let mut total = 0;
        for batch_index in 0..4 {
            let batch = select_variant_batch(plan.clone(), batch_index, 4);
            total += batch.len();
            for variant in batch {
                assert!(observed.insert(variant.name));
            }
        }
        assert_eq!(total, plan.len());
        assert_eq!(observed, expected);
    }

    #[test]
    fn rust_merger_combines_batch_evidence_and_diagnostics() {
        let make_result = |quality: f64, evidence_count: usize, examined_variants: usize| {
            let mut score_components = BTreeMap::new();
            score_components.insert("decoder".into(), 100.0);
            score_components.insert("imageVariant".into(), quality);
            score_components.insert(
                "independentEvidence".into(),
                evidence_count.saturating_sub(1) as f64 * 4.0,
            );
            RecoveryResult {
                status: "decoded".into(),
                candidates: vec![RecoveryCandidate {
                    payload: b"same payload".to_vec(),
                    text: "same payload".into(),
                    version: 1,
                    ec_level: "M".into(),
                    mask: 0,
                    matrix: vec![0; 21 * 21],
                    matrix_kind: "exact".into(),
                    corrected_symbols: 0,
                    score: score_components.values().sum(),
                    score_components,
                    confidence: "high".into(),
                    source: format!("batch-quality-{quality}"),
                    evidence_count,
                }],
                diagnostics: RecoveryDiagnostics {
                    examined_variants,
                    valid_reads: evidence_count,
                    invalid_reads: 1,
                    soft_decode_attempts: 2,
                    elapsed_seconds: quality,
                    input_count: 1,
                    runtime: "rust-wasm+photon+rxing+rqrr".into(),
                },
                discarded_frames: Vec::new(),
                termination_reason: None,
                best_variant: Some(PreviewImage {
                    width: 1,
                    height: 1,
                    data: vec![quality as u8; 4],
                }),
                best_variant_name: Some(format!("quality-{quality}")),
            }
        };
        let merged =
            merge_recovery_results_native(vec![make_result(1.0, 2, 10), make_result(3.0, 3, 12)])
                .unwrap();
        assert_eq!(merged.status, "decoded");
        assert_eq!(merged.diagnostics.examined_variants, 22);
        assert_eq!(merged.diagnostics.valid_reads, 5);
        assert_eq!(merged.candidates[0].evidence_count, 5);
        assert_eq!(merged.best_variant_name.as_deref(), Some("quality-3"));
    }

    #[test]
    fn invalid_hints_are_rejected_before_recovery() {
        let input = RecoveryOptionsInput {
            effort: "balanced".into(),
            max_seconds: None,
            version: Some(41),
            ec_level: None,
            payload_prefix: None,
            payload_regex: None,
            expected_text: None,
            fallback_encoding: None,
            batch_index: None,
            batch_count: None,
        };
        assert!(RecoveryOptions::compile(input).is_err());
    }

    #[test]
    fn synthetic_blurred_regression_is_recovered_by_rust() {
        let path = format!(
            "{}/../../examples/synthetic-blurred.png",
            env!("CARGO_MANIFEST_DIR")
        );
        let encoded = std::fs::read(path).unwrap();
        let decoded = PhotonImage::new_from_byteslice(encoded);
        let observation = WasmImage::from_pixels(
            decoded.get_raw_pixels(),
            decoded.get_width(),
            decoded.get_height(),
        );
        let options = RecoveryOptions::compile(RecoveryOptionsInput {
            effort: "balanced".into(),
            max_seconds: Some(60.0),
            version: None,
            ec_level: None,
            payload_prefix: None,
            payload_regex: None,
            expected_text: None,
            fallback_encoding: None,
            batch_index: None,
            batch_count: None,
        })
        .unwrap();
        let result = recover_native(&[observation], &options, |_, _, _, _| {}).unwrap();
        assert_eq!(
            result.status,
            "decoded",
            "diagnostics: {:?}",
            (
                result.diagnostics.examined_variants,
                result.diagnostics.valid_reads,
                result.diagnostics.invalid_reads,
            )
        );
        assert_eq!(
            result.candidates[0].text,
            "https://qrcode.toolbox.icu/demo/blurred"
        );
        assert!(result.diagnostics.examined_variants < 102);
        assert!(result.candidates[0].evidence_count >= 1);
    }

    #[test]
    fn parallel_batch_returns_after_its_first_verified_variant() {
        let path = format!(
            "{}/../../examples/synthetic-blurred.png",
            env!("CARGO_MANIFEST_DIR")
        );
        let encoded = std::fs::read(path).unwrap();
        let decoded = PhotonImage::new_from_byteslice(encoded);
        let pixels = decoded.get_raw_pixels();
        let width = decoded.get_width();
        let height = decoded.get_height();

        let mut decoded_batch = None;
        for batch_index in 0..4 {
            let observation = WasmImage::from_pixels(pixels.clone(), width, height);
            let options = RecoveryOptions::compile(RecoveryOptionsInput {
                effort: "balanced".into(),
                max_seconds: Some(60.0),
                version: None,
                ec_level: None,
                payload_prefix: None,
                payload_regex: None,
                expected_text: None,
                fallback_encoding: None,
                batch_index: Some(batch_index),
                batch_count: Some(4),
            })
            .unwrap();
            let result = recover_native(&[observation], &options, |_, _, _, _| {}).unwrap();
            if result.status == "decoded" {
                decoded_batch = Some(result);
                break;
            }
        }

        let result = decoded_batch.expect("one parallel batch should decode the regression QR");
        assert_eq!(
            result.termination_reason.as_deref(),
            Some("confidence_limit")
        );
        assert_eq!(
            result.candidates[0].text,
            "https://qrcode.toolbox.icu/demo/blurred"
        );
        assert!(result.diagnostics.examined_variants < 26);
    }

    #[test]
    fn synthetic_deformed_regression_is_recovered_by_rust() {
        let path = format!(
            "{}/../../examples/synthetic-deformed.png",
            env!("CARGO_MANIFEST_DIR")
        );
        let encoded = std::fs::read(path).unwrap();
        let decoded = PhotonImage::new_from_byteslice(encoded);
        let observation = WasmImage::from_pixels(
            decoded.get_raw_pixels(),
            decoded.get_width(),
            decoded.get_height(),
        );
        let options = RecoveryOptions::compile(RecoveryOptionsInput {
            effort: "balanced".into(),
            max_seconds: Some(20.0),
            version: None,
            ec_level: None,
            payload_prefix: None,
            payload_regex: None,
            expected_text: None,
            fallback_encoding: None,
            batch_index: None,
            batch_count: None,
        })
        .unwrap();
        let result = recover_native(&[observation], &options, |_, _, _, _| {}).unwrap();
        assert_eq!(
            result.status,
            "decoded",
            "diagnostics: {:?}; candidates: {:?}",
            (
                result.diagnostics.examined_variants,
                result.diagnostics.valid_reads,
                result.diagnostics.invalid_reads,
            ),
            result
                .candidates
                .iter()
                .map(|candidate| (
                    &candidate.text,
                    candidate.version,
                    &candidate.ec_level,
                    candidate.mask,
                    candidate.score,
                    candidate.evidence_count,
                    &candidate.score_components,
                ))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            result.candidates[0].text,
            "https://qrcode.toolbox.icu/demo/deformed"
        );
        assert_eq!(result.candidates[0].matrix_kind, "exact");
        assert!(result.candidates[0].evidence_count >= 2);
    }
}
