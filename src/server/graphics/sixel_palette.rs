const VT340_PERCENT: [[u32; 3]; 16] = [
    [0, 0, 0],
    [20, 20, 80],
    [80, 13, 13],
    [20, 80, 20],
    [80, 20, 80],
    [20, 80, 80],
    [80, 80, 20],
    [53, 53, 53],
    [26, 26, 26],
    [33, 33, 60],
    [60, 26, 26],
    [33, 60, 33],
    [60, 33, 60],
    [33, 60, 60],
    [60, 60, 33],
    [80, 80, 80],
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    colors: Vec<[u8; 3]>,
}

impl Palette {
    pub const MAX_REGISTERS: usize = 1024;

    pub fn new(registers: usize) -> Self {
        let colors = (0..registers.clamp(1, Self::MAX_REGISTERS))
            .map(|register| {
                VT340_PERCENT
                    .get(register)
                    .map_or([0; 3], |percent| percent.map(percent_to_byte))
            })
            .collect();
        Self { colors }
    }

    pub fn registers(&self) -> usize {
        self.colors.len()
    }

    pub fn color(&self, register: u32) -> [u8; 3] {
        self.colors[self.slot(register)]
    }

    pub fn set_rgb(&mut self, register: u32, red: u32, green: u32, blue: u32) {
        let slot = self.slot(register);
        self.colors[slot] = [red, green, blue].map(percent_to_byte);
    }

    pub fn set_hls(&mut self, register: u32, hue: u32, lightness: u32, saturation: u32) {
        let slot = self.slot(register);
        self.colors[slot] = hls_to_rgb(hue, lightness, saturation);
    }

    fn slot(&self, register: u32) -> usize {
        register as usize % self.colors.len()
    }
}

impl Default for Palette {
    fn default() -> Self {
        Self::new(Self::MAX_REGISTERS)
    }
}

fn percent_to_byte(percent: u32) -> u8 {
    ((percent.min(100) * 255 + 50) / 100) as u8
}

fn hls_to_rgb(hue: u32, lightness: u32, saturation: u32) -> [u8; 3] {
    let hue = f64::from((hue.min(360) + 240) % 360);
    let lightness = f64::from(lightness.min(100)) / 100.0;
    let saturation = f64::from(saturation.min(100)) / 100.0;
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let sector = hue / 60.0;
    let middle = chroma * (1.0 - (sector % 2.0 - 1.0).abs());
    let (red, green, blue) = match sector as u32 {
        0 => (chroma, middle, 0.0),
        1 => (middle, chroma, 0.0),
        2 => (0.0, chroma, middle),
        3 => (0.0, middle, chroma),
        4 => (middle, 0.0, chroma),
        _ => (chroma, 0.0, middle),
    };
    let base = lightness - chroma / 2.0;
    [red, green, blue].map(|channel| ((channel + base) * 255.0).round() as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLACK: [u8; 3] = [0, 0, 0];
    const WHITE: [u8; 3] = [255, 255, 255];
    const RED: [u8; 3] = [255, 0, 0];
    const GREEN: [u8; 3] = [0, 255, 0];
    const BLUE: [u8; 3] = [0, 0, 255];

    fn hls(hue: u32, lightness: u32, saturation: u32) -> [u8; 3] {
        let mut palette = Palette::new(1);
        palette.set_hls(0, hue, lightness, saturation);
        palette.color(0)
    }

    fn rgb(red: u32, green: u32, blue: u32) -> [u8; 3] {
        let mut palette = Palette::new(1);
        palette.set_rgb(0, red, green, blue);
        palette.color(0)
    }

    #[test]
    fn the_default_palette_is_the_vt340_one_followed_by_black() {
        let palette = Palette::default();
        assert_eq!(palette.registers(), 1024);
        assert_eq!(palette.color(0), BLACK);
        assert_eq!(palette.color(1), [51, 51, 204]);
        assert_eq!(palette.color(2), [204, 33, 33]);
        assert_eq!(palette.color(3), [51, 204, 51]);
        assert_eq!(palette.color(7), [135, 135, 135]);
        assert_eq!(palette.color(8), [66, 66, 66]);
        assert_eq!(palette.color(9), [84, 84, 153]);
        assert_eq!(palette.color(15), [204, 204, 204]);
        assert_eq!(palette.color(16), BLACK);
        assert_eq!(palette.color(1023), BLACK);
    }

    #[test]
    fn the_register_count_is_configurable_within_bounds() {
        assert_eq!(Palette::new(16).registers(), 16);
        assert_eq!(Palette::new(256).registers(), 256);
        assert_eq!(Palette::new(0).registers(), 1);
        assert_eq!(Palette::new(usize::MAX).registers(), Palette::MAX_REGISTERS);
        assert_eq!(Palette::new(4).color(3), [51, 204, 51]);
    }

    #[test]
    fn register_indexes_wrap_around_the_register_count() {
        let mut palette = Palette::new(16);
        palette.set_rgb(17, 100, 0, 0);
        assert_eq!(palette.color(1), RED);
        assert_eq!(palette.color(33), RED);
        palette.set_hls(u32::MAX, 120, 50, 100);
        assert_eq!(palette.color(15), RED);
    }

    #[test]
    fn hls_hues_put_blue_at_zero_red_at_120_and_green_at_240() {
        assert_eq!(hls(0, 50, 100), BLUE);
        assert_eq!(hls(120, 50, 100), RED);
        assert_eq!(hls(240, 50, 100), GREEN);
        assert_eq!(hls(360, 50, 100), BLUE);
        assert_eq!(hls(60, 50, 100), [255, 0, 255]);
        assert_eq!(hls(180, 50, 100), [255, 255, 0]);
        assert_eq!(hls(300, 50, 100), [0, 255, 255]);
    }

    #[test]
    fn hls_lightness_and_saturation_are_percentages() {
        assert_eq!(hls(0, 0, 100), BLACK);
        assert_eq!(hls(0, 100, 100), WHITE);
        assert_eq!(hls(120, 25, 100), [128, 0, 0]);
        assert_eq!(hls(120, 75, 100), [255, 128, 128]);
        assert_eq!(hls(120, 50, 50), [191, 64, 64]);
    }

    #[test]
    fn hls_without_saturation_is_grey() {
        assert_eq!(hls(0, 50, 0), [128, 128, 128]);
        assert_eq!(hls(200, 50, 0), [128, 128, 128]);
        assert_eq!(hls(0, 20, 0), [51, 51, 51]);
        assert_eq!(hls(90, 100, 0), WHITE);
    }

    #[test]
    fn rgb_percentages_round_to_the_nearest_byte() {
        assert_eq!(rgb(0, 50, 100), [0, 128, 255]);
        assert_eq!(rgb(1, 33, 99), [3, 84, 252]);
        assert_eq!(rgb(20, 40, 60), [51, 102, 153]);
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        assert_eq!(rgb(101, 1000, u32::MAX), WHITE);
        assert_eq!(hls(u32::MAX, 50, u32::MAX), BLUE);
        assert_eq!(hls(120, 400, 100), WHITE);
    }
}
