//! Visual regression testing — compare screenshots against baselines.
//!
//! Decodes PNG images, computes per-pixel RGBA diffs, and generates diff
//! images highlighting changes. Baselines are stored in a `snapshots/`
//! directory alongside test files.

use std::path::{Path, PathBuf};

use base64::Engine;

use crate::error::TestError;

/// A rectangular region to exclude from pixel comparison.
#[derive(Debug, Clone)]
pub struct MaskRegion {
    /// X coordinate of the top-left corner.
    pub x: u32,
    /// Y coordinate of the top-left corner.
    pub y: u32,
    /// Width of the region.
    pub width: u32,
    /// Height of the region.
    pub height: u32,
}

impl MaskRegion {
    /// Create a new mask region.
    #[must_use]
    pub fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    fn contains(&self, px: u32, py: u32) -> bool {
        px >= self.x && px < self.x + self.width && py >= self.y && py < self.y + self.height
    }
}

/// Named threshold presets for common use cases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThresholdPreset {
    /// Pixel-perfect: tolerance 0, threshold 0%.
    Strict,
    /// Default: tolerance 2, threshold 0.1%.
    Standard,
    /// Tolerant of anti-aliasing and subpixel rendering: tolerance 5, threshold 0.5%.
    AntiAlias,
    /// Lenient for cross-platform use: tolerance 10, threshold 2.0%.
    Relaxed,
}

impl ThresholdPreset {
    fn channel_tolerance(self) -> u8 {
        match self {
            Self::Strict => 0,
            Self::Standard => 2,
            Self::AntiAlias => 5,
            Self::Relaxed => 10,
        }
    }

    fn threshold_percent(self) -> f64 {
        match self {
            Self::Strict => 0.0,
            Self::Standard => 0.1,
            Self::AntiAlias => 0.5,
            Self::Relaxed => 2.0,
        }
    }
}

/// Result of comparing two screenshots pixel-by-pixel.
#[derive(Debug)]
pub struct VisualDiff {
    /// Percentage of pixels that matched (0.0 to 100.0).
    pub match_percentage: f64,
    /// Total number of pixels that differed beyond tolerance.
    pub diff_pixel_count: usize,
    /// Total pixels compared (excludes masked regions).
    pub total_pixels: usize,
    /// Number of pixels skipped by mask regions.
    pub masked_pixels: usize,
    /// Path to the diff image, if one was generated.
    pub diff_image_path: Option<PathBuf>,
}

impl VisualDiff {
    /// Returns true if the images match within the given threshold.
    #[must_use]
    pub fn is_match(&self, threshold_percent: f64) -> bool {
        self.match_percentage >= (100.0 - threshold_percent)
    }
}

/// Options for visual regression comparison.
#[derive(Debug, Clone)]
pub struct VisualOptions {
    /// Directory where baseline snapshots are stored.
    pub snapshot_dir: PathBuf,
    /// Per-channel tolerance (0-255). Pixels differing by less than this
    /// in all channels are considered matching.
    pub channel_tolerance: u8,
    /// Maximum allowed diff percentage before comparison fails.
    pub threshold_percent: f64,
    /// Whether to generate a diff image on mismatch.
    pub generate_diff_image: bool,
    /// Whether to update baselines instead of comparing.
    pub update_baselines: bool,
    /// Rectangular regions to exclude from comparison.
    pub mask_regions: Vec<MaskRegion>,
    /// Store baselines in a platform-specific subdirectory
    /// (e.g., `tests/snapshots/windows/`). Enabled by default.
    pub platform_baselines: bool,
}

impl Default for VisualOptions {
    fn default() -> Self {
        Self {
            snapshot_dir: PathBuf::from("tests/snapshots"),
            channel_tolerance: 2,
            threshold_percent: 0.1,
            generate_diff_image: true,
            update_baselines: false,
            mask_regions: Vec::new(),
            platform_baselines: true,
        }
    }
}

impl VisualOptions {
    /// Apply a threshold preset, overriding `channel_tolerance` and
    /// `threshold_percent`.
    #[must_use]
    pub fn with_preset(mut self, preset: ThresholdPreset) -> Self {
        self.channel_tolerance = preset.channel_tolerance();
        self.threshold_percent = preset.threshold_percent();
        self
    }

    /// Add a mask region to exclude from comparison.
    #[must_use]
    pub fn with_mask(mut self, region: MaskRegion) -> Self {
        self.mask_regions.push(region);
        self
    }

    fn effective_snapshot_dir(&self) -> PathBuf {
        if self.platform_baselines {
            self.snapshot_dir.join(std::env::consts::OS)
        } else {
            self.snapshot_dir.clone()
        }
    }
}

/// Compares a screenshot (base64 PNG) against a stored baseline.
///
/// On first run (no baseline exists), saves the screenshot as the new baseline
/// and returns a perfect match. On subsequent runs, decodes both PNGs and
/// compares pixel-by-pixel.
///
/// # Errors
///
/// Returns [`TestError::VisualRegression`] if the diff exceeds the threshold,
/// or [`TestError::Other`] for IO/decode failures.
pub fn compare_screenshot(
    name: &str,
    screenshot_base64: &str,
    options: &VisualOptions,
) -> Result<VisualDiff, TestError> {
    let screenshot_bytes = base64::engine::general_purpose::STANDARD
        .decode(screenshot_base64)
        .map_err(|e| TestError::Other(format!("failed to decode base64 screenshot: {e}")))?;

    let snap_dir = options.effective_snapshot_dir();
    std::fs::create_dir_all(&snap_dir)
        .map_err(|e| TestError::Other(format!("failed to create snapshot dir: {e}")))?;

    let baseline_path = snap_dir.join(format!("{name}.png"));

    if options.update_baselines || !baseline_path.exists() {
        std::fs::write(&baseline_path, &screenshot_bytes)
            .map_err(|e| TestError::Other(format!("failed to write baseline: {e}")))?;

        return Ok(VisualDiff {
            match_percentage: 100.0,
            diff_pixel_count: 0,
            total_pixels: 0,
            masked_pixels: 0,
            diff_image_path: None,
        });
    }

    let baseline_bytes = std::fs::read(&baseline_path)
        .map_err(|e| TestError::Other(format!("failed to read baseline: {e}")))?;

    let current = decode_png(&screenshot_bytes)?;
    let baseline = decode_png(&baseline_bytes)?;

    if current.width != baseline.width || current.height != baseline.height {
        return Err(TestError::Other(format!(
            "screenshot size {}x{} doesn't match baseline {}x{}",
            current.width, current.height, baseline.width, baseline.height
        )));
    }

    let (diff, masked) = compute_diff(
        &current,
        &baseline,
        options.channel_tolerance,
        &options.mask_regions,
    );
    let total_pixels = (current.width * current.height) as usize - masked;
    let match_percentage = if total_pixels == 0 {
        100.0
    } else {
        (1.0 - diff.len() as f64 / total_pixels as f64) * 100.0
    };

    let diff_image_path = if !diff.is_empty() && options.generate_diff_image {
        let diff_path = snap_dir.join(format!("{name}.diff.png"));
        write_diff_image(&diff_path, &current, &diff)?;
        Some(diff_path)
    } else {
        None
    };

    let result = VisualDiff {
        match_percentage,
        diff_pixel_count: diff.len(),
        total_pixels,
        masked_pixels: masked,
        diff_image_path,
    };

    if !result.is_match(options.threshold_percent) {
        return Err(TestError::VisualRegression(format!(
            "visual regression: {:.2}% pixels differ (threshold: {:.2}%)",
            100.0 - match_percentage,
            options.threshold_percent
        )));
    }

    Ok(result)
}

struct DecodedImage {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

fn decode_png(data: &[u8]) -> Result<DecodedImage, TestError> {
    let decoder = png::Decoder::new(std::io::Cursor::new(data));
    let mut reader = decoder
        .read_info()
        .map_err(|e| TestError::Other(format!("PNG decode error: {e}")))?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| TestError::Other(format!("PNG frame error: {e}")))?;

    let rgba = match info.color_type {
        png::ColorType::Rgba => buf[..info.buffer_size()].to_vec(),
        png::ColorType::Rgb => {
            let rgb = &buf[..info.buffer_size()];
            let mut rgba = Vec::with_capacity(rgb.len() / 3 * 4);
            for chunk in rgb.chunks_exact(3) {
                rgba.extend_from_slice(chunk);
                rgba.push(255);
            }
            rgba
        }
        png::ColorType::Grayscale => {
            let gray = &buf[..info.buffer_size()];
            let mut rgba = Vec::with_capacity(gray.len() * 4);
            for &g in gray {
                rgba.extend_from_slice(&[g, g, g, 255]);
            }
            rgba
        }
        other => {
            return Err(TestError::Other(format!(
                "unsupported PNG color type: {other:?}"
            )));
        }
    };

    Ok(DecodedImage {
        width: info.width,
        height: info.height,
        rgba,
    })
}

fn compute_diff(
    current: &DecodedImage,
    baseline: &DecodedImage,
    tolerance: u8,
    masks: &[MaskRegion],
) -> (Vec<usize>, usize) {
    let mut diff_positions = Vec::new();
    let mut masked_count = 0usize;
    let pixel_count = (current.width * current.height) as usize;

    for i in 0..pixel_count {
        let offset = i * 4;
        if offset + 3 >= current.rgba.len() || offset + 3 >= baseline.rgba.len() {
            break;
        }

        if !masks.is_empty() {
            let px = (i as u32) % current.width;
            let py = (i as u32) / current.width;
            if masks.iter().any(|m| m.contains(px, py)) {
                masked_count += 1;
                continue;
            }
        }

        let dr = current.rgba[offset].abs_diff(baseline.rgba[offset]);
        let dg = current.rgba[offset + 1].abs_diff(baseline.rgba[offset + 1]);
        let db = current.rgba[offset + 2].abs_diff(baseline.rgba[offset + 2]);
        let da = current.rgba[offset + 3].abs_diff(baseline.rgba[offset + 3]);

        if dr > tolerance || dg > tolerance || db > tolerance || da > tolerance {
            diff_positions.push(i);
        }
    }

    (diff_positions, masked_count)
}

fn write_diff_image(
    path: &Path,
    source: &DecodedImage,
    diff_positions: &[usize],
) -> Result<(), TestError> {
    let mut diff_rgba = source.rgba.clone();

    for &pos in diff_positions {
        let offset = pos * 4;
        if offset + 3 < diff_rgba.len() {
            diff_rgba[offset] = 255; // R
            diff_rgba[offset + 1] = 0; // G
            diff_rgba[offset + 2] = 0; // B
            diff_rgba[offset + 3] = 255; // A
        }
    }

    let file = std::fs::File::create(path)
        .map_err(|e| TestError::Other(format!("failed to create diff image: {e}")))?;
    let w = &mut std::io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(w, source.width, source.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|e| TestError::Other(format!("PNG encode error: {e}")))?;
    writer
        .write_image_data(&diff_rgba)
        .map_err(|e| TestError::Other(format!("PNG write error: {e}")))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_solid_png(width: u32, height: u32, r: u8, g: u8, b: u8) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut buf, width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            let mut data = Vec::with_capacity((width * height * 4) as usize);
            for _ in 0..(width * height) {
                data.extend_from_slice(&[r, g, b, 255]);
            }
            writer.write_image_data(&data).unwrap();
        }
        buf
    }

    fn to_base64(data: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(data)
    }

    #[test]
    fn identical_images_match() {
        let dir = tempfile::tempdir().unwrap();
        let png = make_solid_png(10, 10, 128, 128, 128);
        let b64 = to_base64(&png);

        let opts = VisualOptions {
            snapshot_dir: dir.path().to_path_buf(),
            platform_baselines: false,
            ..VisualOptions::default()
        };

        // First run saves baseline
        let result = compare_screenshot("test_identical", &b64, &opts).unwrap();
        assert_eq!(result.match_percentage, 100.0);

        // Second run compares — should match
        let result = compare_screenshot("test_identical", &b64, &opts).unwrap();
        assert_eq!(result.match_percentage, 100.0);
        assert_eq!(result.diff_pixel_count, 0);
    }

    #[test]
    fn different_images_detected() {
        let dir = tempfile::tempdir().unwrap();
        let baseline = make_solid_png(10, 10, 128, 128, 128);
        let changed = make_solid_png(10, 10, 255, 0, 0);

        let opts = VisualOptions {
            snapshot_dir: dir.path().to_path_buf(),
            generate_diff_image: true,
            threshold_percent: 0.1,
            platform_baselines: false,
            ..VisualOptions::default()
        };

        // Save baseline
        compare_screenshot("test_diff", &to_base64(&baseline), &opts).unwrap();

        // Compare with different image — should fail
        let err = compare_screenshot("test_diff", &to_base64(&changed), &opts).unwrap_err();
        match err {
            TestError::VisualRegression(msg) => {
                assert!(msg.contains("visual regression"), "got: {msg}");
            }
            other => panic!("expected VisualRegression, got: {other:?}"),
        }

        // Diff image should exist
        assert!(dir.path().join("test_diff.diff.png").exists());
    }

    #[test]
    fn tolerance_allows_minor_diffs() {
        let dir = tempfile::tempdir().unwrap();
        let baseline = make_solid_png(10, 10, 128, 128, 128);
        let slightly_off = make_solid_png(10, 10, 129, 128, 128);

        let opts = VisualOptions {
            snapshot_dir: dir.path().to_path_buf(),
            channel_tolerance: 2,
            threshold_percent: 1.0,
            platform_baselines: false,
            ..VisualOptions::default()
        };

        compare_screenshot("test_tol", &to_base64(&baseline), &opts).unwrap();
        let result = compare_screenshot("test_tol", &to_base64(&slightly_off), &opts).unwrap();
        assert_eq!(result.match_percentage, 100.0);
    }

    #[test]
    fn update_baselines_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let first = make_solid_png(5, 5, 100, 100, 100);
        let second = make_solid_png(5, 5, 200, 200, 200);

        let mut opts = VisualOptions {
            snapshot_dir: dir.path().to_path_buf(),
            platform_baselines: false,
            ..VisualOptions::default()
        };

        compare_screenshot("test_update", &to_base64(&first), &opts).unwrap();

        opts.update_baselines = true;
        let result = compare_screenshot("test_update", &to_base64(&second), &opts).unwrap();
        assert_eq!(result.match_percentage, 100.0);

        // Now compare without update — should match the new baseline
        opts.update_baselines = false;
        let result = compare_screenshot("test_update", &to_base64(&second), &opts).unwrap();
        assert_eq!(result.match_percentage, 100.0);
    }

    #[test]
    fn size_mismatch_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let small = make_solid_png(5, 5, 128, 128, 128);
        let big = make_solid_png(10, 10, 128, 128, 128);

        let opts = VisualOptions {
            snapshot_dir: dir.path().to_path_buf(),
            platform_baselines: false,
            ..VisualOptions::default()
        };

        compare_screenshot("test_size", &to_base64(&small), &opts).unwrap();
        let err = compare_screenshot("test_size", &to_base64(&big), &opts).unwrap_err();
        match err {
            TestError::Other(msg) => assert!(msg.contains("size"), "got: {msg}"),
            other => panic!("expected Other, got: {other:?}"),
        }
    }

    #[test]
    fn first_run_creates_baseline() {
        let dir = tempfile::tempdir().unwrap();
        let png = make_solid_png(3, 3, 64, 64, 64);

        let opts = VisualOptions {
            snapshot_dir: dir.path().to_path_buf(),
            platform_baselines: false,
            ..VisualOptions::default()
        };

        assert!(!dir.path().join("new_test.png").exists());
        compare_screenshot("new_test", &to_base64(&png), &opts).unwrap();
        assert!(dir.path().join("new_test.png").exists());
    }

    fn make_rgb_png(width: u32, height: u32, r: u8, g: u8, b: u8) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut buf, width, height);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            let mut data = Vec::with_capacity((width * height * 3) as usize);
            for _ in 0..(width * height) {
                data.extend_from_slice(&[r, g, b]);
            }
            writer.write_image_data(&data).unwrap();
        }
        buf
    }

    fn make_grayscale_png(width: u32, height: u32, value: u8) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut buf, width, height);
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            let data = vec![value; (width * height) as usize];
            writer.write_image_data(&data).unwrap();
        }
        buf
    }

    #[test]
    fn rgb_png_converts_to_rgba() {
        let dir = tempfile::tempdir().unwrap();
        // Save baseline as RGBA (the standard path)
        let baseline = make_solid_png(8, 8, 200, 100, 50);
        // Produce the "screenshot" as RGB (triggers the RGB→RGBA branch)
        let screenshot = make_rgb_png(8, 8, 200, 100, 50);

        let opts = VisualOptions {
            snapshot_dir: dir.path().to_path_buf(),
            channel_tolerance: 0,
            threshold_percent: 0.1,
            platform_baselines: false,
            ..VisualOptions::default()
        };

        compare_screenshot("rgb_test", &to_base64(&baseline), &opts).unwrap();
        let result = compare_screenshot("rgb_test", &to_base64(&screenshot), &opts).unwrap();
        assert_eq!(result.match_percentage, 100.0);
        assert_eq!(result.diff_pixel_count, 0);
    }

    #[test]
    fn grayscale_png_converts_to_rgba() {
        let dir = tempfile::tempdir().unwrap();
        let gray_value: u8 = 128;
        // Save baseline as RGBA with equivalent gray (r=g=b=128, a=255)
        let baseline = make_solid_png(6, 6, gray_value, gray_value, gray_value);
        // Produce the "screenshot" as Grayscale (triggers the Grayscale→RGBA branch)
        let screenshot = make_grayscale_png(6, 6, gray_value);

        let opts = VisualOptions {
            snapshot_dir: dir.path().to_path_buf(),
            channel_tolerance: 0,
            threshold_percent: 0.1,
            platform_baselines: false,
            ..VisualOptions::default()
        };

        compare_screenshot("gray_test", &to_base64(&baseline), &opts).unwrap();
        let result = compare_screenshot("gray_test", &to_base64(&screenshot), &opts).unwrap();
        assert_eq!(result.match_percentage, 100.0);
        assert_eq!(result.diff_pixel_count, 0);
    }

    #[test]
    fn is_match_threshold_logic() {
        let diff = VisualDiff {
            match_percentage: 99.5,
            diff_pixel_count: 5,
            total_pixels: 1000,
            masked_pixels: 0,
            diff_image_path: None,
        };
        // threshold 1.0 → needs >= 99.0 → 99.5 passes
        assert!(diff.is_match(1.0));
        // threshold 0.5 → needs >= 99.5 → 99.5 passes (exact boundary)
        assert!(diff.is_match(0.5));
        // threshold 0.1 → needs >= 99.9 → 99.5 fails
        assert!(!diff.is_match(0.1));
    }

    #[test]
    fn mask_region_excludes_pixels() {
        let dir = tempfile::tempdir().unwrap();
        let baseline = make_solid_png(10, 10, 128, 128, 128);
        // Every pixel is different — but we mask the entire image
        let changed = make_solid_png(10, 10, 255, 0, 0);

        let opts = VisualOptions {
            snapshot_dir: dir.path().to_path_buf(),
            threshold_percent: 0.1,
            mask_regions: vec![MaskRegion::new(0, 0, 10, 10)],
            platform_baselines: false,
            ..VisualOptions::default()
        };

        compare_screenshot("mask_all", &to_base64(&baseline), &opts).unwrap();
        let result = compare_screenshot("mask_all", &to_base64(&changed), &opts).unwrap();
        assert_eq!(result.match_percentage, 100.0);
        assert_eq!(result.masked_pixels, 100);
        assert_eq!(result.diff_pixel_count, 0);
    }

    #[test]
    fn mask_region_partial_exclusion() {
        let dir = tempfile::tempdir().unwrap();
        // 4x4 image — mask the top-left 2x2 quadrant (4 pixels)
        let baseline = make_solid_png(4, 4, 100, 100, 100);
        let changed = make_solid_png(4, 4, 200, 200, 200);

        let opts = VisualOptions {
            snapshot_dir: dir.path().to_path_buf(),
            channel_tolerance: 0,
            threshold_percent: 100.0,
            mask_regions: vec![MaskRegion::new(0, 0, 2, 2)],
            platform_baselines: false,
            ..VisualOptions::default()
        };

        compare_screenshot("mask_partial", &to_base64(&baseline), &opts).unwrap();
        let result = compare_screenshot("mask_partial", &to_base64(&changed), &opts).unwrap();
        assert_eq!(result.masked_pixels, 4);
        // 16 total - 4 masked = 12 compared, all 12 differ
        assert_eq!(result.diff_pixel_count, 12);
        assert_eq!(result.total_pixels, 12);
    }

    #[test]
    fn threshold_preset_strict() {
        let opts = VisualOptions::default().with_preset(ThresholdPreset::Strict);
        assert_eq!(opts.channel_tolerance, 0);
        assert!((opts.threshold_percent - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn threshold_preset_relaxed() {
        let opts = VisualOptions::default().with_preset(ThresholdPreset::Relaxed);
        assert_eq!(opts.channel_tolerance, 10);
        assert!((opts.threshold_percent - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn threshold_preset_anti_alias() {
        let opts = VisualOptions::default().with_preset(ThresholdPreset::AntiAlias);
        assert_eq!(opts.channel_tolerance, 5);
        assert!((opts.threshold_percent - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn platform_baselines_creates_os_subdir() {
        let dir = tempfile::tempdir().unwrap();
        let png = make_solid_png(4, 4, 64, 64, 64);

        let opts = VisualOptions {
            snapshot_dir: dir.path().to_path_buf(),
            platform_baselines: true,
            ..VisualOptions::default()
        };

        compare_screenshot("plattest", &to_base64(&png), &opts).unwrap();
        let expected = dir.path().join(std::env::consts::OS).join("plattest.png");
        assert!(expected.exists(), "baseline not at {}", expected.display());
    }

    #[test]
    fn platform_baselines_disabled_uses_root() {
        let dir = tempfile::tempdir().unwrap();
        let png = make_solid_png(4, 4, 64, 64, 64);

        let opts = VisualOptions {
            snapshot_dir: dir.path().to_path_buf(),
            platform_baselines: false,
            ..VisualOptions::default()
        };

        compare_screenshot("noplattest", &to_base64(&png), &opts).unwrap();
        let expected = dir.path().join("noplattest.png");
        assert!(expected.exists(), "baseline not at {}", expected.display());
        // Ensure no OS subdir was created
        assert!(!dir.path().join(std::env::consts::OS).exists());
    }

    #[test]
    fn with_mask_builder_chains() {
        let opts = VisualOptions::default()
            .with_mask(MaskRegion::new(0, 0, 50, 50))
            .with_mask(MaskRegion::new(100, 100, 25, 25));
        assert_eq!(opts.mask_regions.len(), 2);
    }
}
