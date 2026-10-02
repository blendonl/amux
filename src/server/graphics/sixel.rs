use std::mem;

use super::sixel_palette::Palette;

const BAND_HEIGHT: u32 = 6;
const MAX_PARAMS: usize = 5;
const UNSET: [u8; 4] = [0; 4];

type Column = [[u8; 4]; BAND_HEIGHT as usize];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Background {
    #[default]
    Opaque,
    Transparent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SixelParams {
    pub background: Background,
}

impl SixelParams {
    pub fn from_dcs(params: &[u16]) -> Self {
        let background = match params.get(1) {
            Some(1) => Background::Transparent,
            _ => Background::Opaque,
        };
        Self { background }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SixelLimits {
    pub max_width: u32,
    pub max_height: u32,
}

impl Default for SixelLimits {
    fn default() -> Self {
        Self {
            max_width: 4096,
            max_height: 4096,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SixelImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Repeat,
    Color,
    Raster,
}

#[derive(Debug, Clone, Copy, Default)]
struct Extent {
    width: u32,
    height: u32,
}

#[derive(Debug)]
pub struct SixelDecoder {
    background: Background,
    palette: Palette,
    limits: SixelLimits,
    state: State,
    params: [u32; MAX_PARAMS],
    param: usize,
    register: u32,
    x: u32,
    band: u32,
    bands: Vec<Vec<Column>>,
    started: bool,
    declared: Extent,
    painted: Extent,
}

impl SixelDecoder {
    pub fn new(params: SixelParams, palette: Palette, limits: SixelLimits) -> Self {
        Self {
            background: params.background,
            palette,
            limits,
            state: State::Ground,
            params: [0; MAX_PARAMS],
            param: 0,
            register: 0,
            x: 0,
            band: 0,
            bands: Vec::new(),
            started: false,
            declared: Extent::default(),
            painted: Extent::default(),
        }
    }

    pub fn put(&mut self, byte: u8) {
        match (self.state, byte) {
            (State::Ground, _) => self.ground(byte),
            (_, b'0'..=b'9') => self.push_digit(byte - b'0'),
            (State::Color | State::Raster, b';') => self.param = (self.param + 1).min(MAX_PARAMS),
            (State::Repeat, b'?'..=b'~') => {
                self.state = State::Ground;
                self.paint(byte, self.params[0].max(1));
            }
            _ => {
                self.end_command();
                self.ground(byte);
            }
        }
    }

    pub fn finish(mut self) -> (Option<SixelImage>, Palette) {
        self.end_command();
        let image = self.image();
        (image, self.palette)
    }

    fn ground(&mut self, byte: u8) {
        match byte {
            b'?'..=b'~' => self.paint(byte, 1),
            b'$' => self.x = 0,
            b'-' => {
                self.x = 0;
                self.band = (self.band + 1).min(self.limits.max_height.div_ceil(BAND_HEIGHT));
            }
            b'!' => self.begin(State::Repeat),
            b'#' => self.begin(State::Color),
            b'"' => self.begin(State::Raster),
            _ => {}
        }
    }

    fn begin(&mut self, state: State) {
        self.state = state;
        self.params = [0; MAX_PARAMS];
        self.param = 0;
    }

    fn push_digit(&mut self, digit: u8) {
        if let Some(value) = self.params.get_mut(self.param) {
            *value = value.saturating_mul(10).saturating_add(u32::from(digit));
        }
    }

    fn end_command(&mut self) {
        match mem::replace(&mut self.state, State::Ground) {
            State::Color => self.set_color(),
            State::Raster => self.set_raster(),
            State::Ground | State::Repeat => {}
        }
    }

    fn set_color(&mut self) {
        let [register, space, x, y, z] = self.params;
        self.register = register;
        if self.param + 1 < MAX_PARAMS {
            return;
        }
        match space {
            1 => self.palette.set_hls(register, x, y, z),
            2 => self.palette.set_rgb(register, x, y, z),
            _ => {}
        }
    }

    fn set_raster(&mut self) {
        if self.started {
            return;
        }
        let [_, _, width, height, _] = self.params;
        self.declared = Extent {
            width: width.min(self.limits.max_width),
            height: height.min(self.limits.max_height),
        };
    }

    fn paint(&mut self, byte: u8, count: u32) {
        self.started = true;
        let start = self.x;
        let end = start.saturating_add(count).min(self.limits.max_width);
        self.x = end;
        let bits = (byte - b'?') & self.band_mask();
        if bits == 0 || start == end {
            return;
        }
        let color = opaque(self.palette.color(self.register));
        for column in &mut self.columns(end)[start as usize..] {
            for (row, pixel) in column.iter_mut().enumerate() {
                if bits & (1 << row) != 0 {
                    *pixel = color;
                }
            }
        }
        self.painted.width = self.painted.width.max(end);
        let bottom = self.band * BAND_HEIGHT + (u8::BITS - bits.leading_zeros());
        self.painted.height = self.painted.height.max(bottom);
    }

    fn band_mask(&self) -> u8 {
        let top = self.band.saturating_mul(BAND_HEIGHT);
        let rows = self.limits.max_height.saturating_sub(top).min(BAND_HEIGHT);
        (1 << rows) - 1
    }

    fn columns(&mut self, width: u32) -> &mut [Column] {
        let band = self.band as usize;
        if self.bands.len() <= band {
            self.bands.resize_with(band + 1, Vec::new);
        }
        let columns = &mut self.bands[band];
        let width = width as usize;
        if columns.capacity() < width {
            let capacity = (columns.capacity() * 2).clamp(width, self.limits.max_width as usize);
            columns.reserve_exact(capacity - columns.len());
        }
        if columns.len() < width {
            columns.resize(width, [UNSET; BAND_HEIGHT as usize]);
        }
        &mut columns[..width]
    }

    fn image(&self) -> Option<SixelImage> {
        let width = self.declared.width.max(self.painted.width);
        let height = self.declared.height.max(self.painted.height);
        if width == 0 || height == 0 {
            return None;
        }
        let background = match self.background {
            Background::Opaque => opaque(self.palette.color(0)),
            Background::Transparent => UNSET,
        };
        let stride = width as usize * background.len();
        let mut rgba = background.repeat(width as usize * height as usize);
        for (y, row) in rgba.chunks_exact_mut(stride).enumerate() {
            let Some(columns) = self.bands.get(y / BAND_HEIGHT as usize) else {
                break;
            };
            for (pixel, column) in row.chunks_exact_mut(background.len()).zip(columns) {
                let painted = column[y % BAND_HEIGHT as usize];
                if painted != UNSET {
                    pixel.copy_from_slice(&painted);
                }
            }
        }
        Some(SixelImage {
            width,
            height,
            rgba,
        })
    }
}

fn opaque([red, green, blue]: [u8; 3]) -> [u8; 4] {
    [red, green, blue, u8::MAX]
}

#[cfg(test)]
mod tests {
    use super::*;

    const HI: &[u8] = include_bytes!("fixtures/hi.six");
    const HLS: &[u8] = include_bytes!("fixtures/hls.six");
    const SHAPES: &[u8] = include_bytes!("fixtures/shapes.six");
    const SHAPES_RGBA: &[u8] = include_bytes!("fixtures/shapes.rgba");
    const FIXTURES: [&[u8]; 3] = [HI, HLS, SHAPES];

    const CLEAR: [u8; 4] = [0, 0, 0, 0];
    const BLACK: [u8; 4] = [0, 0, 0, 255];
    const RED: [u8; 4] = [255, 0, 0, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];
    const YELLOW: [u8; 4] = [255, 255, 0, 255];
    const VT340_BLUE: [u8; 4] = [51, 51, 204, 255];
    const VT340_RED: [u8; 4] = [204, 33, 33, 255];

    const TRANSPARENT: SixelParams = SixelParams {
        background: Background::Transparent,
    };

    fn decode_with(
        params: SixelParams,
        palette: Palette,
        limits: SixelLimits,
        bytes: &[u8],
    ) -> (Option<SixelImage>, Palette) {
        let mut decoder = SixelDecoder::new(params, palette, limits);
        bytes.iter().for_each(|&byte| decoder.put(byte));
        decoder.finish()
    }

    fn decode_as(params: SixelParams, bytes: &[u8]) -> Option<SixelImage> {
        decode_with(params, Palette::default(), SixelLimits::default(), bytes).0
    }

    fn decode(bytes: &[u8]) -> SixelImage {
        decode_as(SixelParams::default(), bytes).unwrap()
    }

    fn picture(image: &SixelImage, legend: &[(char, [u8; 4])]) -> Vec<String> {
        image
            .rgba
            .chunks_exact(image.width as usize * 4)
            .map(|row| {
                row.chunks_exact(4)
                    .map(|pixel| {
                        legend
                            .iter()
                            .find(|(_, color)| pixel == color)
                            .map_or('?', |&(name, _)| name)
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn the_hi_sample_decodes_to_red_letters_on_yellow() {
        for params in [SixelParams::default(), TRANSPARENT] {
            let image = decode_as(params, HI).unwrap();
            assert_eq!((image.width, image.height), (14, 7));
            assert_eq!(image.rgba.len(), 14 * 7 * 4);
            assert_eq!(
                picture(&image, &[('Y', YELLOW), ('R', VT340_RED)]),
                [
                    "YYYYYYYYYYYYYY",
                    "YYRRYYRRYYRRYY",
                    "YYRRYYRRYYRRYY",
                    "YYRRRRRRYYRRYY",
                    "YYRRYYRRYYRRYY",
                    "YYRRYYRRYYRRYY",
                    "YYYYYYYYYYYYYY",
                ]
            );
        }
    }

    #[test]
    fn the_hls_fixture_paints_bands_repeats_and_partial_sixels() {
        let legend = [
            ('R', RED),
            ('G', GREEN),
            ('B', BLUE),
            ('K', BLACK),
            ('.', CLEAR),
        ];
        let image = decode_as(TRANSPARENT, HLS).unwrap();
        assert_eq!((image.width, image.height), (12, 12));
        assert_eq!(
            picture(&image, &legend),
            [
                "RRRRGGGGBBBB",
                "RRRRGGGGBBBB",
                "RRRRGGGGBBBB",
                "RRRRGGGGBBBB",
                "RRRRGGGGBBBB",
                "RRRRGGGGBBBB",
                "BBBBBBRRRRRR",
                "BBBBBBRRRRRR",
                "BBBBBB......",
                "BBBBBB......",
                "...GGG......",
                "...GGG......",
            ]
        );
        let opaque = decode(HLS);
        assert_eq!(picture(&opaque, &legend)[11], "KKKGGGKKKKKK");
    }

    #[test]
    fn an_img2sixel_image_matches_imagemagicks_decoding() {
        let image = decode(SHAPES);
        assert_eq!((image.width, image.height), (23, 17));
        assert_eq!(image.rgba, SHAPES_RGBA);
    }

    #[test]
    fn raster_attributes_extend_the_image_past_the_painted_area() {
        let legend = [('B', VT340_BLUE), ('.', CLEAR), ('K', BLACK)];
        let image = decode_as(TRANSPARENT, b"\"1;1;3;8#1~").unwrap();
        assert_eq!(
            picture(&image, &legend),
            ["B..", "B..", "B..", "B..", "B..", "B..", "...", "..."]
        );
        let opaque = decode(b"\"1;1;3;8#1~");
        assert_eq!(picture(&opaque, &legend)[7], "KKK");
    }

    #[test]
    fn painting_past_the_declared_size_extends_the_image() {
        let image = decode(b"\"1;1;2;2#1!4~-#1@");
        assert_eq!((image.width, image.height), (4, 7));
    }

    #[test]
    fn the_image_ends_at_its_lowest_painted_row_not_the_band_edge() {
        let image = decode(b"#1!3N");
        assert_eq!((image.width, image.height), (3, 4));
        let declared = decode(b"\"1;1;3;5#1!3N");
        assert_eq!((declared.width, declared.height), (3, 5));
        let blank_columns = decode_as(TRANSPARENT, b"#1@!5?");
        assert_eq!(blank_columns.map(|image| image.width), Some(1));
    }

    #[test]
    fn raster_attributes_after_the_first_sixel_are_ignored() {
        let image = decode(b"#1~\"1;1;9;9~");
        assert_eq!((image.width, image.height), (2, 6));
        let after_a_blank_sixel = decode(b"?\"1;1;9;9#1~");
        assert_eq!(
            (after_a_blank_sixel.width, after_a_blank_sixel.height),
            (2, 6)
        );
        let twice_before = decode(b"\"1;1;9;9\"1;1;4;3#1@");
        assert_eq!((twice_before.width, twice_before.height), (4, 3));
    }

    #[test]
    fn the_aspect_ratio_is_always_one_to_one() {
        let image = decode(b"\"5;1;1;1#1~");
        assert_eq!((image.width, image.height), (1, 6));
        let wide = decode(b"\"1;5#1~");
        assert_eq!((wide.width, wide.height), (1, 6));
    }

    #[test]
    fn the_size_is_cropped_to_the_limits() {
        let limits = SixelLimits {
            max_width: 8,
            max_height: 4,
        };
        let size = |bytes: &[u8]| {
            decode_with(SixelParams::default(), Palette::default(), limits, bytes)
                .0
                .map(|image| (image.width, image.height))
        };
        assert_eq!(size(b"\"1;1;100;100"), Some((8, 4)));
        assert_eq!(size(b"#1!20~"), Some((8, 4)));
        assert_eq!(size(b"#1~-#1~"), Some((1, 4)));
    }

    #[test]
    fn unset_pixels_are_register_zero_when_opaque_and_clear_when_transparent() {
        let bytes = b"#0;2;0;0;100\"1;1;2;2#1;2;100;0;0#1@";
        let legend = [('R', RED), ('B', BLUE), ('.', CLEAR)];
        let opaque = decode(bytes);
        assert_eq!(picture(&opaque, &legend), ["RB", "BB"]);
        let transparent = decode_as(TRANSPARENT, bytes).unwrap();
        assert_eq!(picture(&transparent, &legend), ["R.", ".."]);
    }

    #[test]
    fn the_opaque_background_is_register_zero_as_it_is_at_the_end() {
        let image = decode(b"\"1;1;2;1#1@#0;2;0;100;0");
        assert_eq!(picture(&image, &[('B', VT340_BLUE), ('G', GREEN)]), ["BG"]);
    }

    #[test]
    fn dcs_p2_of_one_makes_the_background_transparent() {
        let background = |params: &[u16]| SixelParams::from_dcs(params).background;
        assert_eq!(background(&[]), Background::Opaque);
        assert_eq!(background(&[0]), Background::Opaque);
        assert_eq!(background(&[0, 0]), Background::Opaque);
        assert_eq!(background(&[0, 1]), Background::Transparent);
        assert_eq!(background(&[7, 1, 0]), Background::Transparent);
        assert_eq!(background(&[1, 2, 0]), Background::Opaque);
        assert_eq!(background(&[0, 3]), Background::Opaque);
    }

    #[test]
    fn color_introducers_define_hls_and_rgb_registers() {
        let image = decode(
            b"#1;1;0;50;100#2;1;120;50;100#3;1;240;50;100#4;1;77;50;0\
              #5;2;50;33;1#6;3;1;2;3#7;2;100;0;0;99\
              #1@#2@#3@#4@#5@#6@#7@",
        );
        let pixels: Vec<&[u8]> = image.rgba.chunks_exact(4).collect();
        assert_eq!(
            pixels,
            [
                &BLUE[..],
                &RED,
                &GREEN,
                &[128, 128, 128, 255],
                &[128, 84, 3, 255],
                &[204, 204, 51, 255],
                &RED,
            ]
        );
    }

    #[test]
    fn a_color_introducer_without_a_number_selects_register_zero() {
        let image = decode(b"#0;2;0;100;0#1#@");
        assert_eq!(image.rgba, GREEN);
    }

    #[test]
    fn register_indexes_wrap_around_the_palette() {
        let bytes = b"#17;2;100;0;0#1@#2;2;0;100;0#18@";
        let (image, _) = decode_with(
            SixelParams::default(),
            Palette::new(16),
            SixelLimits::default(),
            bytes,
        );
        assert_eq!(
            picture(&image.unwrap(), &[('R', RED), ('G', GREEN)]),
            ["RG"]
        );
        let saturated = decode(b"#99999999999999999999;2;100;0;0#1023@");
        assert_eq!(saturated.rgba, RED);
    }

    #[test]
    fn redefining_a_register_keeps_the_pixels_already_painted() {
        let image = decode(b"#1;2;100;0;0#1~#1;2;0;100;0~");
        assert_eq!(picture(&image, &[('R', RED), ('G', GREEN)])[0], "RG");
    }

    #[test]
    fn the_palette_returned_by_finish_carries_over_to_the_next_image() {
        let (image, palette) = decode_with(
            SixelParams::default(),
            Palette::new(256),
            SixelLimits::default(),
            b"#5;2;0;0;100",
        );
        assert_eq!(image, None);
        assert_eq!(palette.registers(), 256);
        assert_eq!(palette.color(5), [0, 0, 255]);
        let (image, palette) = decode_with(TRANSPARENT, palette, SixelLimits::default(), b"#5@");
        assert_eq!(image.unwrap().rgba, BLUE);
        assert_eq!(palette.color(5), [0, 0, 255]);
    }

    #[test]
    fn a_command_cut_off_by_the_end_of_the_data_still_applies() {
        let image = decode_as(TRANSPARENT, b"\"1;1;3;2").unwrap();
        assert_eq!((image.width, image.height), (3, 2));
        assert_eq!(image.rgba, [0; 3 * 2 * 4]);
    }

    #[test]
    fn nothing_painted_and_nothing_declared_is_no_image() {
        assert_eq!(decode_as(SixelParams::default(), b""), None);
        assert_eq!(
            decode_as(SixelParams::default(), b"$-#1;2;0;0;0??!9?"),
            None
        );
        assert_eq!(decode_as(SixelParams::default(), b"\"1;1;0;5#1"), None);
    }

    #[test]
    fn whitespace_and_unknown_bytes_are_ignored() {
        let image = decode(b"#1 ~\r\n\t\x00\x1b\x7f\x80\xff%&'()*+,./:<=>~");
        assert_eq!((image.width, image.height), (2, 6));
        assert_eq!(picture(&image, &[('B', VT340_BLUE)])[0], "BB");
    }

    #[test]
    fn a_repeat_without_a_sixel_is_dropped() {
        let image = decode(b"#1!5$~!3#1~!0~");
        assert_eq!((image.width, image.height), (3, 6));
    }

    #[test]
    fn huge_repeat_counts_stop_at_the_width_limit() {
        let mut decoder = SixelDecoder::new(
            SixelParams::default(),
            Palette::default(),
            SixelLimits::default(),
        );
        for &byte in b"#1!999999999~!99999999999999999999~$!4096@" {
            decoder.put(byte);
        }
        assert!(decoder.bands[0].capacity() <= 4096);
        let image = decoder.finish().0.unwrap();
        assert_eq!((image.width, image.height), (4096, 6));
    }

    #[test]
    fn long_runs_of_graphics_new_lines_stay_inside_the_height_limit() {
        let mut decoder = SixelDecoder::new(
            SixelParams::default(),
            Palette::default(),
            SixelLimits::default(),
        );
        for _ in 0..1_000_000 {
            decoder.put(b'-');
        }
        decoder.put(b'~');
        assert!(decoder.bands.is_empty());
        assert_eq!(decoder.finish().0, None);

        let mut bytes = vec![b'-'; 682];
        bytes.push(b'~');
        let mut decoder = SixelDecoder::new(
            SixelParams::default(),
            Palette::default(),
            SixelLimits::default(),
        );
        bytes.iter().for_each(|&byte| decoder.put(byte));
        assert_eq!(decoder.bands.len(), 683);
        let image = decoder.finish().0.unwrap();
        assert_eq!((image.width, image.height), (1, 4096));
    }

    #[test]
    fn fixtures_fed_one_byte_at_a_time_and_interleaved_decode_as_the_whole_slice() {
        for params in [SixelParams::default(), TRANSPARENT] {
            let whole: Vec<_> = FIXTURES
                .iter()
                .map(|fixture| decode_as(params, fixture))
                .collect();
            let mut decoders: Vec<_> = FIXTURES
                .iter()
                .map(|_| SixelDecoder::new(params, Palette::default(), SixelLimits::default()))
                .collect();
            let longest = FIXTURES.iter().map(|fixture| fixture.len()).max();
            for index in 0..longest.unwrap_or(0) {
                for (decoder, fixture) in decoders.iter_mut().zip(FIXTURES) {
                    if let Some(&byte) = fixture.get(index) {
                        decoder.put(byte);
                    }
                }
            }
            let interleaved: Vec<_> = decoders
                .into_iter()
                .map(|decoder| decoder.finish().0)
                .collect();
            assert_eq!(interleaved, whole);
        }
    }

    #[test]
    fn every_fixture_can_be_finished_after_any_byte() {
        for fixture in FIXTURES {
            let whole = decode(fixture);
            for end in 0..fixture.len() {
                let Some(image) = decode_as(SixelParams::default(), &fixture[..end]) else {
                    continue;
                };
                assert!(image.width <= whole.width && image.height <= whole.height);
                assert_eq!(image.rgba.len(), (image.width * image.height * 4) as usize);
            }
        }
    }

    struct Random(u64);

    impl Random {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, bound: u64) -> u64 {
            self.next() % bound
        }
    }

    struct TestImage {
        width: u32,
        height: u32,
        declared: bool,
        colors: Vec<[u32; 3]>,
        pixels: Vec<Option<usize>>,
    }

    impl TestImage {
        fn random(random: &mut Random) -> Self {
            let width = 1 + random.below(40) as u32;
            let height = 1 + random.below(40) as u32;
            let colors = (0..16)
                .map(|_| [0; 3].map(|_| 20 * random.below(6) as u32))
                .collect();
            let holes = random.below(2) == 0;
            let pixels = (0..width * height)
                .map(|_| match random.below(17) as usize {
                    16 if holes => None,
                    register => Some(register % 16),
                })
                .collect();
            Self {
                width,
                height,
                declared: holes || random.below(2) == 0,
                colors,
                pixels,
            }
        }

        fn encode(&self) -> Vec<u8> {
            let mut out = String::new();
            if self.declared {
                out += &format!("\"1;1;{};{}", self.width, self.height);
            }
            for (register, [red, green, blue]) in self.colors.iter().enumerate() {
                out += &format!("#{register};2;{red};{green};{blue}");
            }
            for band in 0..self.height.div_ceil(BAND_HEIGHT) {
                if band > 0 {
                    out.push('-');
                }
                let mut first = true;
                for register in 0..self.colors.len() {
                    let sixels = self.sixels(band, register);
                    if sixels.iter().all(|&sixel| sixel == b'?') {
                        continue;
                    }
                    if !first {
                        out.push('$');
                    }
                    first = false;
                    out += &format!("#{register}");
                    for run in sixels.chunk_by(|left, right| left == right) {
                        let sixel = char::from(run[0]);
                        if run.len() >= 3 {
                            out += &format!("!{}{sixel}", run.len());
                        } else {
                            out.extend(run.iter().map(|&sixel| char::from(sixel)));
                        }
                    }
                }
            }
            out.into_bytes()
        }

        fn sixels(&self, band: u32, register: usize) -> Vec<u8> {
            (0..self.width)
                .map(|x| {
                    (0..BAND_HEIGHT)
                        .map(|row| band * BAND_HEIGHT + row)
                        .filter(|&y| y < self.height)
                        .filter(|&y| self.pixels[(y * self.width + x) as usize] == Some(register))
                        .fold(b'?', |sixel, y| sixel + (1 << (y % BAND_HEIGHT)))
                })
                .collect()
        }

        fn rgba(&self, background: Background) -> Vec<u8> {
            let opaque = |register: usize| {
                let [red, green, blue] =
                    self.colors[register].map(|percent| (percent / 20 * 51) as u8);
                [red, green, blue, 255]
            };
            self.pixels
                .iter()
                .flat_map(|pixel| match (pixel, background) {
                    (Some(register), _) => opaque(*register),
                    (None, Background::Opaque) => opaque(0),
                    (None, Background::Transparent) => CLEAR,
                })
                .collect()
        }
    }

    #[test]
    fn random_sixteen_color_images_survive_an_encode_and_decode() {
        let mut random = Random(0x9e37_79b9_7f4a_7c15);
        for _ in 0..300 {
            let image = TestImage::random(&mut random);
            let background = if random.below(2) == 0 {
                Background::Opaque
            } else {
                Background::Transparent
            };
            let decoded = decode_as(SixelParams { background }, &image.encode()).unwrap();
            assert_eq!((decoded.width, decoded.height), (image.width, image.height));
            assert_eq!(decoded.rgba, image.rgba(background));
        }
    }

    #[test]
    fn random_bytes_never_panic_and_stay_inside_the_limits() {
        let alphabet = b"0123456789;;;;#####!!!!\"\"$$--??@@~~NBo \n\x1b\x7f";
        let mut random = Random(0x2545_f491_4f6c_dd1d);
        for _ in 0..400 {
            let limits = SixelLimits {
                max_width: random.below(64) as u32,
                max_height: random.below(64) as u32,
            };
            let palette = Palette::new(random.below(20) as usize);
            let params = SixelParams::from_dcs(&[0, random.below(3) as u16]);
            let mut decoder = SixelDecoder::new(params, palette, limits);
            for _ in 0..random.below(3000) {
                let byte = match random.below(5) {
                    0 => random.next() as u8,
                    _ => alphabet[random.below(alphabet.len() as u64) as usize],
                };
                decoder.put(byte);
            }
            let Some(image) = decoder.finish().0 else {
                continue;
            };
            assert!(image.width >= 1 && image.width <= limits.max_width);
            assert!(image.height >= 1 && image.height <= limits.max_height);
            assert_eq!(image.rgba.len(), (image.width * image.height * 4) as usize);
        }
    }
}
