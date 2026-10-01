use std::borrow::Cow;
use std::io::Cursor;

use anyhow::{anyhow, bail, Context, Result};
use miniz_oxide::deflate::compress_to_vec_zlib;
use miniz_oxide::inflate::decompress_to_vec_zlib_with_limit;
use png::{Decoder, Limits, Transformations};

use super::place::{CellOffset, SourceRect};
use super::store::ImageData;
use crate::protocol::{CellPixels, ImageFormat};

pub const MAX_SIDE: u32 = 4096;
const MAX_SOURCE_BYTES: usize = 256 * 1024 * 1024;
const DEFLATE_LEVEL: u8 = 1;
const RGBA: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Sizing {
    Native,
    Columns,
    Rows,
    Stretch,
}

impl Sizing {
    pub fn of(columns: u32, rows: u32) -> Self {
        match (columns, rows) {
            (0, 0) => Self::Native,
            (_, 0) => Self::Columns,
            (0, _) => Self::Rows,
            _ => Self::Stretch,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Look {
    pub source: SourceRect,
    pub offset: CellOffset,
    pub sizing: Sizing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    pub width: u32,
    pub height: u32,
    pub source: SourceRect,
    pub dest: Area,
}

impl Look {
    pub fn plan(&self, image: (u32, u32), cells: (u16, u16), cell: CellPixels) -> Option<Plan> {
        let (cols, rows) = cells;
        let width = u32::from(cols) * u32::from(cell.width.max(1));
        let height = u32::from(rows) * u32::from(cell.height.max(1));
        let plan = Plan {
            width,
            height,
            source: self.source,
            dest: self.dest(width, height),
        };
        (!self.fits_plainly(image, width, height)).then_some(plan)
    }

    fn fits_plainly(&self, image: (u32, u32), width: u32, height: u32) -> bool {
        let (image_width, image_height) = image;
        let whole = SourceRect {
            x: 0,
            y: 0,
            width: image_width,
            height: image_height,
        };
        let source_width = u64::from(self.source.width);
        let source_height = u64::from(self.source.height);
        let (width, height) = (u64::from(width), u64::from(height));
        self.source == whole
            && self.offset == CellOffset::default()
            && match self.sizing {
                Sizing::Native => (source_width, source_height) == (width, height),
                Sizing::Columns | Sizing::Rows | Sizing::Stretch => {
                    source_width * height == source_height * width
                }
            }
    }

    fn dest(&self, width: u32, height: u32) -> Area {
        let CellOffset { x, y } = self.offset;
        let SourceRect {
            width: source_width,
            height: source_height,
            ..
        } = self.source;
        let (dest_width, dest_height) = match self.sizing {
            Sizing::Native => (source_width, source_height),
            Sizing::Stretch => (width.saturating_sub(x), height.saturating_sub(y)),
            Sizing::Columns => {
                let dest_width = width.saturating_sub(x);
                (dest_width, scale(dest_width, source_height, source_width))
            }
            Sizing::Rows => {
                let dest_height = height.saturating_sub(y);
                (scale(dest_height, source_width, source_height), dest_height)
            }
        };
        Area {
            x,
            y,
            width: dest_width,
            height: dest_height,
        }
    }
}

impl Plan {
    pub fn fits(&self) -> bool {
        self.width <= MAX_SIDE && self.height <= MAX_SIDE
    }

    pub fn decoded_len(&self) -> usize {
        self.width as usize * self.height as usize * RGBA
    }
}

fn scale(length: u32, numerator: u32, denominator: u32) -> u32 {
    if denominator == 0 {
        return 0;
    }
    let denominator = u64::from(denominator);
    let scaled = (u64::from(length) * u64::from(numerator) + denominator / 2) / denominator;
    u32::try_from(scaled).unwrap_or(u32::MAX)
}

pub fn derive(image: &ImageData, plan: &Plan) -> Result<ImageData> {
    if !plan.fits() {
        bail!(
            "{}x{} is larger than {MAX_SIDE}x{MAX_SIDE}",
            plan.width,
            plan.height
        );
    }
    let pixels = Pixels::decode(image)?;
    let mut canvas = vec![0; plan.decoded_len()];
    pixels.draw(plan, &mut canvas);
    Ok(ImageData {
        key: image.key,
        width: plan.width,
        height: plan.height,
        format: ImageFormat::Rgba32,
        compressed: true,
        bytes: compress_to_vec_zlib(&canvas, DEFLATE_LEVEL).into(),
        decoded_len: canvas.len(),
    })
}

pub fn unpack(image: &ImageData) -> Result<Vec<u8>> {
    let pixels = Pixels::decode(image)?;
    if image.format == ImageFormat::Rgb24 || pixels.channels == RGBA {
        return Ok(pixels.data);
    }
    Ok((0..pixels.height)
        .flat_map(|y| (0..pixels.width).map(move |x| (x, y)))
        .flat_map(|(x, y)| pixels.rgba(x, y))
        .collect())
}

struct Pixels {
    width: u32,
    height: u32,
    channels: usize,
    data: Vec<u8>,
}

struct Tap {
    first: u32,
    weights: Vec<f64>,
}

impl Pixels {
    fn decode(image: &ImageData) -> Result<Self> {
        if image.decoded_len > MAX_SOURCE_BYTES {
            bail!(
                "a {}x{} image is too large to derive from",
                image.width,
                image.height
            );
        }
        let bytes: Cow<'_, [u8]> = if image.compressed {
            decompress_to_vec_zlib_with_limit(&image.bytes, MAX_SOURCE_BYTES)
                .map(Cow::Owned)
                .map_err(|error| anyhow!("can't inflate the image: {error}"))?
        } else {
            Cow::Borrowed(&image.bytes)
        };
        match image.format {
            ImageFormat::Png => Self::from_png(&bytes),
            ImageFormat::Rgb24 => Self::from_raw(image, bytes, 3),
            ImageFormat::Rgba32 => Self::from_raw(image, bytes, 4),
        }
    }

    fn from_png(bytes: &[u8]) -> Result<Self> {
        let limits = Limits {
            bytes: MAX_SOURCE_BYTES,
        };
        let mut decoder = Decoder::new_with_limits(Cursor::new(bytes), limits);
        decoder.set_transformations(Transformations::normalize_to_color8());
        let mut reader = decoder.read_info().context("can't read the PNG header")?;
        let len = reader
            .output_buffer_size()
            .context("the PNG is too large to decode")?;
        let mut data = vec![0; len];
        let frame = reader
            .next_frame(&mut data)
            .context("can't decode the PNG")?;
        data.truncate(frame.buffer_size());
        Ok(Self {
            width: frame.width,
            height: frame.height,
            channels: frame.color_type.samples(),
            data,
        })
    }

    fn from_raw(image: &ImageData, bytes: Cow<'_, [u8]>, channels: usize) -> Result<Self> {
        let len = image.width as usize * image.height as usize * channels;
        if bytes.len() < len {
            bail!("insufficient image data: {} < {len}", bytes.len());
        }
        let mut data = bytes.into_owned();
        data.truncate(len);
        Ok(Self {
            width: image.width,
            height: image.height,
            channels,
            data,
        })
    }

    fn draw(&self, plan: &Plan, canvas: &mut [u8]) {
        let source = self.clip(plan.source);
        let dest = plan.dest;
        let cols = dest.width.min(plan.width.saturating_sub(dest.x));
        let rows = dest.height.min(plan.height.saturating_sub(dest.y));
        if source.width == 0 || source.height == 0 || cols == 0 || rows == 0 {
            return;
        }
        let columns = taps(source.x, source.width, dest.width, cols);
        let lines = taps(source.y, source.height, dest.height, rows);
        let stride = plan.width as usize * RGBA;
        for (row, line) in (dest.y as usize..).zip(&lines) {
            for (col, column) in (dest.x as usize..).zip(&columns) {
                let at = row * stride + col * RGBA;
                canvas[at..at + RGBA].copy_from_slice(&self.average(column, line));
            }
        }
    }

    fn clip(&self, source: SourceRect) -> SourceRect {
        let x = source.x.min(self.width);
        let y = source.y.min(self.height);
        SourceRect {
            x,
            y,
            width: source.width.min(self.width - x),
            height: source.height.min(self.height - y),
        }
    }

    fn average(&self, column: &Tap, line: &Tap) -> [u8; 4] {
        let mut sum = [0.0_f64; 4];
        for (y, line_weight) in (line.first..).zip(&line.weights) {
            for (x, column_weight) in (column.first..).zip(&column.weights) {
                let [red, green, blue, alpha] = self.rgba(x, y);
                let coverage = line_weight * column_weight * f64::from(alpha);
                sum[0] += coverage * f64::from(red);
                sum[1] += coverage * f64::from(green);
                sum[2] += coverage * f64::from(blue);
                sum[3] += coverage;
            }
        }
        let alpha = sum[3];
        if alpha <= 0.0 {
            return [0; 4];
        }
        let channel = |total: f64| (total / alpha).round() as u8;
        [
            channel(sum[0]),
            channel(sum[1]),
            channel(sum[2]),
            alpha.round() as u8,
        ]
    }

    fn rgba(&self, x: u32, y: u32) -> [u8; 4] {
        let at = (y as usize * self.width as usize + x as usize) * self.channels;
        match self.data[at..at + self.channels] {
            [value] => [value, value, value, u8::MAX],
            [value, alpha] => [value, value, value, alpha],
            [red, green, blue] => [red, green, blue, u8::MAX],
            [red, green, blue, alpha] => [red, green, blue, alpha],
            _ => [0; 4],
        }
    }
}

fn taps(start: u32, len: u32, out: u32, visible: u32) -> Vec<Tap> {
    let (start, len, out) = (u64::from(start), u64::from(len), u64::from(out));
    (0..u64::from(visible))
        .map(|at| {
            let low = start * out + at * len;
            let high = low + len;
            let first = low / out;
            let weights = (first..high.div_ceil(out))
                .map(|pixel| {
                    let covered = high.min((pixel + 1) * out) - low.max(pixel * out);
                    covered as f64 / len as f64
                })
                .collect();
            Tap {
                first: u32::try_from(first).unwrap_or(u32::MAX),
                weights,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use miniz_oxide::inflate::decompress_to_vec_zlib;
    use png::{BitDepth, ColorType, Encoder};

    use super::super::store::ImageKey;
    use super::*;

    const CLEAR: [u8; 4] = [0; 4];

    fn cell(width: u16, height: u16) -> CellPixels {
        CellPixels { width, height }
    }

    fn colour(x: u32, y: u32) -> [u8; 4] {
        [
            u8::try_from(x * 40).unwrap(),
            u8::try_from(y * 40).unwrap(),
            7,
            255,
        ]
    }

    fn rgba(width: u32, height: u32) -> ImageData {
        let pixels: Vec<u8> = (0..height)
            .flat_map(|y| (0..width).flat_map(move |x| colour(x, y)))
            .collect();
        raw(ImageFormat::Rgba32, width, height, &pixels)
    }

    fn raw(format: ImageFormat, width: u32, height: u32, pixels: &[u8]) -> ImageData {
        ImageData {
            key: ImageKey(1),
            width,
            height,
            format,
            compressed: true,
            bytes: compress_to_vec_zlib(pixels, 6).into(),
            decoded_len: pixels.len(),
        }
    }

    fn png(width: u32, height: u32, colour_type: ColorType, pixels: &[u8]) -> Vec<u8> {
        let mut png = Vec::new();
        let mut encoder = Encoder::new(&mut png, width, height);
        encoder.set_color(colour_type);
        encoder.set_depth(BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(pixels).unwrap();
        writer.finish().unwrap();
        png
    }

    fn whole(width: u32, height: u32) -> SourceRect {
        SourceRect {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    fn look(source: SourceRect, offset: (u32, u32), sizing: Sizing) -> Look {
        let (x, y) = offset;
        Look {
            source,
            offset: CellOffset { x, y },
            sizing,
        }
    }

    fn derived(image: &ImageData, look: Look, cells: (u16, u16), cell: CellPixels) -> Vec<[u8; 4]> {
        let plan = look
            .plan((image.width, image.height), cells, cell)
            .expect("the look needs a derived image");
        let derived = derive(image, &plan).unwrap();
        let (cols, rows) = cells;
        assert_eq!(
            (derived.width, derived.height),
            (
                u32::from(cols) * u32::from(cell.width),
                u32::from(rows) * u32::from(cell.height)
            )
        );
        assert_eq!(
            (derived.format, derived.compressed),
            (ImageFormat::Rgba32, true)
        );
        let bytes = decompress_to_vec_zlib(&derived.bytes).unwrap();
        assert_eq!(bytes.len(), derived.decoded_len);
        bytes
            .chunks(RGBA)
            .map(|pixel| pixel.try_into().unwrap())
            .collect()
    }

    #[test]
    fn a_crop_shows_only_the_source_rectangle() {
        let image = rgba(4, 4);
        let crop = SourceRect {
            x: 1,
            y: 1,
            width: 2,
            height: 2,
        };
        assert_eq!(
            derived(
                &image,
                look(crop, (0, 0), Sizing::Native),
                (2, 1),
                cell(1, 2)
            ),
            [colour(1, 1), colour(2, 1), colour(1, 2), colour(2, 2)]
        );
        assert_eq!(
            derived(
                &image,
                look(crop, (0, 0), Sizing::Stretch),
                (2, 1),
                cell(2, 4)
            ),
            [
                [colour(1, 1), colour(1, 1), colour(2, 1), colour(2, 1)],
                [colour(1, 1), colour(1, 1), colour(2, 1), colour(2, 1)],
                [colour(1, 2), colour(1, 2), colour(2, 2), colour(2, 2)],
                [colour(1, 2), colour(1, 2), colour(2, 2), colour(2, 2)],
            ]
            .concat()
        );
    }

    #[test]
    fn a_cell_offset_shifts_the_pixels_and_leaves_clear_margins() {
        let image = rgba(2, 2);
        assert_eq!(
            derived(
                &image,
                look(whole(2, 2), (1, 2), Sizing::Native),
                (2, 2),
                cell(2, 2)
            ),
            [
                [CLEAR; 4],
                [CLEAR; 4],
                [CLEAR, colour(0, 0), colour(1, 0), CLEAR],
                [CLEAR, colour(0, 1), colour(1, 1), CLEAR],
            ]
            .concat()
        );
    }

    #[test]
    fn both_cell_counts_stretch_the_image_to_fill_them() {
        let image = raw(ImageFormat::Rgb24, 2, 1, &[200, 0, 0, 0, 0, 100]);
        let red = [200, 0, 0, 255];
        let blue = [0, 0, 100, 255];
        assert_eq!(
            derived(
                &image,
                look(whole(2, 1), (0, 0), Sizing::Stretch),
                (2, 2),
                cell(1, 1)
            ),
            [red, blue, red, blue]
        );
        assert_eq!(
            derived(
                &image,
                look(whole(2, 1), (1, 0), Sizing::Stretch),
                (2, 2),
                cell(2, 1)
            ),
            [
                [CLEAR, red, [100, 0, 50, 255], blue],
                [CLEAR, red, [100, 0, 50, 255], blue],
            ]
            .concat()
        );
    }

    #[test]
    fn a_native_size_image_smaller_than_its_cells_stays_top_left() {
        let image = rgba(1, 1);
        assert_eq!(
            derived(
                &image,
                look(whole(1, 1), (0, 0), Sizing::Native),
                (1, 1),
                cell(2, 2)
            ),
            [colour(0, 0), CLEAR, CLEAR, CLEAR]
        );
    }

    #[test]
    fn one_cell_count_scales_the_other_side_and_keeps_the_top_left() {
        let image = rgba(2, 1);
        assert_eq!(
            derived(
                &image,
                look(whole(2, 1), (0, 0), Sizing::Columns),
                (2, 1),
                cell(2, 3)
            ),
            [
                [colour(0, 0), colour(0, 0), colour(1, 0), colour(1, 0)],
                [colour(0, 0), colour(0, 0), colour(1, 0), colour(1, 0)],
                [CLEAR; 4],
            ]
            .concat()
        );
        let tall = rgba(1, 2);
        assert_eq!(
            derived(
                &tall,
                look(whole(1, 2), (0, 0), Sizing::Rows),
                (1, 1),
                cell(2, 2)
            ),
            [colour(0, 0), CLEAR, colour(0, 1), CLEAR]
        );
    }

    #[test]
    fn shrinking_averages_and_keeps_the_colour_of_half_clear_pixels() {
        let pixels = [
            [255, 0, 0, 255],
            CLEAR,
            [0, 0, 0, 255],
            [100, 100, 100, 255],
        ]
        .concat();
        let image = raw(ImageFormat::Rgba32, 4, 1, &pixels);
        assert_eq!(
            derived(
                &image,
                look(whole(4, 1), (0, 0), Sizing::Stretch),
                (2, 1),
                cell(1, 1)
            ),
            [[255, 0, 0, 128], [50, 50, 50, 255]]
        );
    }

    #[test]
    fn png_sources_are_decoded_whatever_their_colour_type() {
        let rgb = [10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120];
        let sent = png(2, 2, ColorType::Rgb, &rgb);
        let image = ImageData {
            key: ImageKey(1),
            width: 2,
            height: 2,
            format: ImageFormat::Png,
            compressed: false,
            bytes: sent.clone().into(),
            decoded_len: 16,
        };
        let column = SourceRect {
            x: 1,
            y: 0,
            width: 1,
            height: 2,
        };
        let right = look(column, (0, 0), Sizing::Native);
        let expected = [[40, 50, 60, 255], [100, 110, 120, 255]];
        assert_eq!(derived(&image, right, (1, 1), cell(1, 2)), expected);

        let zipped = ImageData {
            compressed: true,
            bytes: compress_to_vec_zlib(&sent, 6).into(),
            ..image
        };
        assert_eq!(derived(&zipped, right, (1, 1), cell(1, 2)), expected);

        let grey = ImageData {
            bytes: png(2, 1, ColorType::GrayscaleAlpha, &[9, 255, 77, 0]).into(),
            height: 1,
            ..image
        };
        assert_eq!(
            derived(
                &grey,
                look(whole(2, 1), (0, 1), Sizing::Native),
                (2, 1),
                cell(1, 2)
            ),
            [CLEAR, CLEAR, [9, 9, 9, 255], CLEAR]
        );
    }

    #[test]
    fn broken_sources_fail_to_derive() {
        let plan = look(whole(2, 2), (1, 1), Sizing::Native)
            .plan((2, 2), (1, 1), cell(4, 4))
            .unwrap();
        let mut image = rgba(2, 2);
        image.bytes = compress_to_vec_zlib(&[1, 2, 3], 6).into();
        assert!(derive(&image, &plan).is_err());
        image.bytes = b"not zlib".to_vec().into();
        assert!(derive(&image, &plan).is_err());
        image.format = ImageFormat::Png;
        image.compressed = false;
        assert!(derive(&image, &plan).is_err());
    }

    #[test]
    fn only_looks_the_plain_fit_gets_wrong_are_derived() {
        let plan = |source, offset, sizing, cells, size| {
            look(source, offset, sizing).plan(size, cells, cell(10, 20))
        };
        let full = whole(20, 40);
        assert_eq!(plan(full, (0, 0), Sizing::Native, (2, 2), (20, 40)), None);
        assert_eq!(plan(full, (0, 0), Sizing::Stretch, (4, 4), (20, 40)), None);
        assert_eq!(plan(full, (0, 0), Sizing::Columns, (1, 1), (20, 40)), None);
        assert_eq!(plan(full, (0, 0), Sizing::Rows, (3, 3), (20, 40)), None);

        assert!(plan(full, (0, 0), Sizing::Native, (2, 2), (20, 41)).is_some());
        assert!(plan(whole(15, 40), (0, 0), Sizing::Native, (2, 2), (15, 40)).is_some());
        assert!(plan(full, (0, 0), Sizing::Stretch, (4, 2), (20, 40)).is_some());
        assert!(plan(full, (0, 0), Sizing::Columns, (2, 3), (20, 40)).is_some());
        assert!(plan(full, (1, 0), Sizing::Native, (3, 2), (20, 40)).is_some());
        assert!(plan(full, (0, 1), Sizing::Native, (2, 3), (20, 40)).is_some());

        let cropped = SourceRect {
            x: 0,
            y: 0,
            width: 10,
            height: 20,
        };
        let crop = plan(cropped, (0, 0), Sizing::Native, (1, 1), (20, 40)).unwrap();
        assert_eq!(
            crop,
            Plan {
                width: 10,
                height: 20,
                source: cropped,
                dest: Area {
                    x: 0,
                    y: 0,
                    width: 10,
                    height: 20
                },
            }
        );
        assert!(crop.fits());
    }

    #[test]
    fn a_derived_image_is_capped_at_4096_pixels_a_side() {
        let image = rgba(1, 1);
        let wide = look(whole(1, 1), (0, 0), Sizing::Stretch)
            .plan((1, 1), (297, 1), cell(14, 28))
            .unwrap();
        assert_eq!((wide.width, wide.fits()), (4158, false));
        assert!(derive(&image, &wide).is_err());
        let edge = look(whole(1, 1), (0, 0), Sizing::Stretch)
            .plan((1, 1), (256, 1), cell(16, 32))
            .unwrap();
        assert_eq!((edge.width, edge.fits()), (4096, true));
    }
}
