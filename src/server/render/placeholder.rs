use vt100::Color;

use super::grid::{Cell, Style, TEXT_CAPACITY};

pub const PLACEHOLDER: char = '\u{10eeee}';

pub const MAX_IMAGE_CELLS: u16 = 297;

pub const DIACRITICS: [char; MAX_IMAGE_CELLS as usize] = [
    '\u{305}',
    '\u{30d}',
    '\u{30e}',
    '\u{310}',
    '\u{312}',
    '\u{33d}',
    '\u{33e}',
    '\u{33f}',
    '\u{346}',
    '\u{34a}',
    '\u{34b}',
    '\u{34c}',
    '\u{350}',
    '\u{351}',
    '\u{352}',
    '\u{357}',
    '\u{35b}',
    '\u{363}',
    '\u{364}',
    '\u{365}',
    '\u{366}',
    '\u{367}',
    '\u{368}',
    '\u{369}',
    '\u{36a}',
    '\u{36b}',
    '\u{36c}',
    '\u{36d}',
    '\u{36e}',
    '\u{36f}',
    '\u{483}',
    '\u{484}',
    '\u{485}',
    '\u{486}',
    '\u{487}',
    '\u{592}',
    '\u{593}',
    '\u{594}',
    '\u{595}',
    '\u{597}',
    '\u{598}',
    '\u{599}',
    '\u{59c}',
    '\u{59d}',
    '\u{59e}',
    '\u{59f}',
    '\u{5a0}',
    '\u{5a1}',
    '\u{5a8}',
    '\u{5a9}',
    '\u{5ab}',
    '\u{5ac}',
    '\u{5af}',
    '\u{5c4}',
    '\u{610}',
    '\u{611}',
    '\u{612}',
    '\u{613}',
    '\u{614}',
    '\u{615}',
    '\u{616}',
    '\u{617}',
    '\u{657}',
    '\u{658}',
    '\u{659}',
    '\u{65a}',
    '\u{65b}',
    '\u{65d}',
    '\u{65e}',
    '\u{6d6}',
    '\u{6d7}',
    '\u{6d8}',
    '\u{6d9}',
    '\u{6da}',
    '\u{6db}',
    '\u{6dc}',
    '\u{6df}',
    '\u{6e0}',
    '\u{6e1}',
    '\u{6e2}',
    '\u{6e4}',
    '\u{6e7}',
    '\u{6e8}',
    '\u{6eb}',
    '\u{6ec}',
    '\u{730}',
    '\u{732}',
    '\u{733}',
    '\u{735}',
    '\u{736}',
    '\u{73a}',
    '\u{73d}',
    '\u{73f}',
    '\u{740}',
    '\u{741}',
    '\u{743}',
    '\u{745}',
    '\u{747}',
    '\u{749}',
    '\u{74a}',
    '\u{7eb}',
    '\u{7ec}',
    '\u{7ed}',
    '\u{7ee}',
    '\u{7ef}',
    '\u{7f0}',
    '\u{7f1}',
    '\u{7f3}',
    '\u{816}',
    '\u{817}',
    '\u{818}',
    '\u{819}',
    '\u{81b}',
    '\u{81c}',
    '\u{81d}',
    '\u{81e}',
    '\u{81f}',
    '\u{820}',
    '\u{821}',
    '\u{822}',
    '\u{823}',
    '\u{825}',
    '\u{826}',
    '\u{827}',
    '\u{829}',
    '\u{82a}',
    '\u{82b}',
    '\u{82c}',
    '\u{82d}',
    '\u{951}',
    '\u{953}',
    '\u{954}',
    '\u{f82}',
    '\u{f83}',
    '\u{f86}',
    '\u{f87}',
    '\u{135d}',
    '\u{135e}',
    '\u{135f}',
    '\u{17dd}',
    '\u{193a}',
    '\u{1a17}',
    '\u{1a75}',
    '\u{1a76}',
    '\u{1a77}',
    '\u{1a78}',
    '\u{1a79}',
    '\u{1a7a}',
    '\u{1a7b}',
    '\u{1a7c}',
    '\u{1b6b}',
    '\u{1b6d}',
    '\u{1b6e}',
    '\u{1b6f}',
    '\u{1b70}',
    '\u{1b71}',
    '\u{1b72}',
    '\u{1b73}',
    '\u{1cd0}',
    '\u{1cd1}',
    '\u{1cd2}',
    '\u{1cda}',
    '\u{1cdb}',
    '\u{1ce0}',
    '\u{1dc0}',
    '\u{1dc1}',
    '\u{1dc3}',
    '\u{1dc4}',
    '\u{1dc5}',
    '\u{1dc6}',
    '\u{1dc7}',
    '\u{1dc8}',
    '\u{1dc9}',
    '\u{1dcb}',
    '\u{1dcc}',
    '\u{1dd1}',
    '\u{1dd2}',
    '\u{1dd3}',
    '\u{1dd4}',
    '\u{1dd5}',
    '\u{1dd6}',
    '\u{1dd7}',
    '\u{1dd8}',
    '\u{1dd9}',
    '\u{1dda}',
    '\u{1ddb}',
    '\u{1ddc}',
    '\u{1ddd}',
    '\u{1dde}',
    '\u{1ddf}',
    '\u{1de0}',
    '\u{1de1}',
    '\u{1de2}',
    '\u{1de3}',
    '\u{1de4}',
    '\u{1de5}',
    '\u{1de6}',
    '\u{1dfe}',
    '\u{20d0}',
    '\u{20d1}',
    '\u{20d4}',
    '\u{20d5}',
    '\u{20d6}',
    '\u{20d7}',
    '\u{20db}',
    '\u{20dc}',
    '\u{20e1}',
    '\u{20e7}',
    '\u{20e9}',
    '\u{20f0}',
    '\u{2cef}',
    '\u{2cf0}',
    '\u{2cf1}',
    '\u{2de0}',
    '\u{2de1}',
    '\u{2de2}',
    '\u{2de3}',
    '\u{2de4}',
    '\u{2de5}',
    '\u{2de6}',
    '\u{2de7}',
    '\u{2de8}',
    '\u{2de9}',
    '\u{2dea}',
    '\u{2deb}',
    '\u{2dec}',
    '\u{2ded}',
    '\u{2dee}',
    '\u{2def}',
    '\u{2df0}',
    '\u{2df1}',
    '\u{2df2}',
    '\u{2df3}',
    '\u{2df4}',
    '\u{2df5}',
    '\u{2df6}',
    '\u{2df7}',
    '\u{2df8}',
    '\u{2df9}',
    '\u{2dfa}',
    '\u{2dfb}',
    '\u{2dfc}',
    '\u{2dfd}',
    '\u{2dfe}',
    '\u{2dff}',
    '\u{a66f}',
    '\u{a67c}',
    '\u{a67d}',
    '\u{a6f0}',
    '\u{a6f1}',
    '\u{a8e0}',
    '\u{a8e1}',
    '\u{a8e2}',
    '\u{a8e3}',
    '\u{a8e4}',
    '\u{a8e5}',
    '\u{a8e6}',
    '\u{a8e7}',
    '\u{a8e8}',
    '\u{a8e9}',
    '\u{a8ea}',
    '\u{a8eb}',
    '\u{a8ec}',
    '\u{a8ed}',
    '\u{a8ee}',
    '\u{a8ef}',
    '\u{a8f0}',
    '\u{a8f1}',
    '\u{aab0}',
    '\u{aab2}',
    '\u{aab3}',
    '\u{aab7}',
    '\u{aab8}',
    '\u{aabe}',
    '\u{aabf}',
    '\u{aac1}',
    '\u{fe20}',
    '\u{fe21}',
    '\u{fe22}',
    '\u{fe23}',
    '\u{fe24}',
    '\u{fe25}',
    '\u{fe26}',
    '\u{10a0f}',
    '\u{10a38}',
    '\u{1d185}',
    '\u{1d186}',
    '\u{1d187}',
    '\u{1d188}',
    '\u{1d189}',
    '\u{1d1aa}',
    '\u{1d1ab}',
    '\u{1d1ac}',
    '\u{1d1ad}',
    '\u{1d242}',
    '\u{1d243}',
    '\u{1d244}',
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageSpan {
    pub key: u32,
    pub image_row: u16,
    pub image_col: u16,
    pub row: u16,
    pub col: u16,
    pub cols: u16,
}

pub fn placeholder_cell(key: u32, image_row: u16, image_col: u16, background: Color) -> Cell {
    let [high, red, green, blue] = key.to_be_bytes();
    let mut text = [0; TEXT_CAPACITY];
    let mut len = 0;
    for mark in [
        PLACEHOLDER,
        clamped_diacritic(image_row),
        clamped_diacritic(image_col),
        clamped_diacritic(u16::from(high)),
    ] {
        len += mark.encode_utf8(&mut text[len..]).len();
    }
    Cell::with_text(
        std::str::from_utf8(&text[..len]).unwrap_or_default(),
        Style {
            fg: Color::Rgb(red, green, blue),
            bg: background,
            ..Style::default()
        },
    )
}

fn clamped_diacritic(index: u16) -> char {
    DIACRITICS[usize::from(index.min(MAX_IMAGE_CELLS - 1))]
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use unicode_width::UnicodeWidthChar;

    use super::*;

    fn text(key: u32, image_row: u16, image_col: u16) -> String {
        placeholder_cell(key, image_row, image_col, Color::Default)
            .text()
            .to_owned()
    }

    #[test]
    fn the_diacritics_are_kittys_row_and_column_marks() {
        assert_eq!(DIACRITICS.len(), 297);
        assert_eq!(DIACRITICS[..3], ['\u{305}', '\u{30d}', '\u{30e}']);
        assert_eq!(DIACRITICS[296], '\u{1d244}');
        assert_eq!(DIACRITICS.iter().collect::<BTreeSet<_>>().len(), 297);
        assert!(DIACRITICS.iter().all(|mark| mark.width() == Some(0)));
        assert_eq!(PLACEHOLDER.width(), Some(1));
    }

    #[test]
    fn a_placeholder_spells_the_row_the_column_and_the_high_byte_of_the_key() {
        assert_eq!(
            text(0x0012_3456, 0, 0).as_bytes(),
            b"\xf4\x8e\xbb\xae\xcc\x85\xcc\x85\xcc\x85"
        );
        assert_eq!(
            text(0x2a12_3456, 1, 2).as_bytes(),
            b"\xf4\x8e\xbb\xae\xcc\x8d\xcc\x8e\xd6\x9c"
        );
        assert_eq!(
            text(u32::MAX, 296, 296).as_bytes(),
            b"\xf4\x8e\xbb\xae\xf0\x9d\x89\x84\xf0\x9d\x89\x84\xea\xa3\xa5"
        );
        assert_eq!(
            text(0x0100_0000, 3, 7),
            ['\u{10eeee}', '\u{310}', '\u{33f}', '\u{30d}']
                .iter()
                .collect::<String>()
        );
    }

    #[test]
    fn a_placeholder_is_coloured_with_the_low_bytes_of_the_key_and_nothing_else() {
        let cell = placeholder_cell(0x2a12_3456, 4, 5, Color::Idx(4));
        assert_eq!(
            cell.style(),
            Style {
                fg: Color::Rgb(0x12, 0x34, 0x56),
                bg: Color::Idx(4),
                ..Style::default()
            }
        );
        assert!(!cell.is_wide() && !cell.is_wide_continuation() && !cell.is_erased());
    }

    #[test]
    fn rows_and_columns_past_the_last_diacritic_are_clamped_to_it() {
        let last = MAX_IMAGE_CELLS - 1;
        assert_eq!(last, 296);
        for (row, col) in [(297, 0), (0, 297), (1000, u16::MAX)] {
            assert_eq!(
                text(7, row, col),
                text(7, row.min(last), col.min(last)),
                "({row}, {col})"
            );
        }
        assert_ne!(text(7, 295, 0), text(7, 296, 0));
    }

    #[test]
    fn the_longest_placeholder_fits_in_a_cell() {
        let widest = DIACRITICS.iter().map(|mark| mark.len_utf8()).max().unwrap();
        assert!(PLACEHOLDER.len_utf8() + 3 * widest <= TEXT_CAPACITY);
        for key in [0, 0xff_ffff, 0x0100_0000, u32::MAX] {
            for index in [0, 1, 255, 282, 283, 296] {
                assert_eq!(text(key, index, index).chars().count(), 4);
            }
        }
    }
}
