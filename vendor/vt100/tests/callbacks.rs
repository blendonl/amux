#[derive(Debug, PartialEq, Eq)]
enum Event {
    EraseInDisplay(u16),
    Reset {
        contents: String,
    },
    AlternateScreen {
        entered: bool,
        cleared: bool,
        active: bool,
    },
    Hook {
        params: Vec<Vec<u16>>,
        intermediates: Vec<u8>,
        ignore: bool,
        action: char,
    },
    Put(u8),
    Unhook,
    Csi(char),
}

#[derive(Default)]
struct Recorder(Vec<Event>);

impl vt100::Callbacks for Recorder {
    fn erase_in_display(&mut self, _: &mut vt100::Screen, mode: u16) {
        self.0.push(Event::EraseInDisplay(mode));
    }

    fn reset(&mut self, screen: &mut vt100::Screen) {
        self.0.push(Event::Reset {
            contents: screen.contents(),
        });
    }

    fn alternate_screen(
        &mut self,
        screen: &mut vt100::Screen,
        entered: bool,
        cleared: bool,
    ) {
        self.0.push(Event::AlternateScreen {
            entered,
            cleared,
            active: screen.alternate_screen(),
        });
    }

    fn dcs_hook(
        &mut self,
        _: &mut vt100::Screen,
        params: &[&[u16]],
        intermediates: &[u8],
        ignore: bool,
        action: char,
    ) {
        self.0.push(Event::Hook {
            params: params.iter().map(|param| param.to_vec()).collect(),
            intermediates: intermediates.to_vec(),
            ignore,
            action,
        });
    }

    fn dcs_put(&mut self, _: &mut vt100::Screen, byte: u8) {
        self.0.push(Event::Put(byte));
    }

    fn dcs_unhook(&mut self, _: &mut vt100::Screen) {
        self.0.push(Event::Unhook);
    }

    fn unhandled_csi(
        &mut self,
        _: &mut vt100::Screen,
        _i1: Option<u8>,
        _i2: Option<u8>,
        _params: &[&[u16]],
        c: char,
    ) {
        self.0.push(Event::Csi(c));
    }
}

fn parser() -> vt100::Parser<Recorder> {
    vt100::Parser::new_with_callbacks(4, 10, 0, Recorder::default())
}

fn events(chunks: &[&[u8]]) -> Vec<Event> {
    let mut parser = parser();
    for chunk in chunks {
        parser.process(chunk);
    }
    std::mem::take(&mut parser.callbacks_mut().0)
}

const SIXEL: &[u8] = b"\x1bP0;1;0q#0;2;0;0;0#0~~\x1b\\";

fn sixel_events() -> Vec<Event> {
    let mut expected = vec![Event::Hook {
        params: vec![vec![0], vec![1], vec![0]],
        intermediates: vec![],
        ignore: false,
        action: 'q',
    }];
    expected.extend(b"#0;2;0;0;0#0~~".iter().map(|&byte| Event::Put(byte)));
    expected.push(Event::Unhook);
    expected
}

#[test]
fn erase_in_display_reports_every_mode_after_erasing() {
    assert_eq!(
        events(&[b"\x1b[J\x1b[0J\x1b[1J\x1b[2J\x1b[3J\x1b[?2J\x1b[4J"]),
        [
            Event::EraseInDisplay(0),
            Event::EraseInDisplay(0),
            Event::EraseInDisplay(1),
            Event::EraseInDisplay(2),
            Event::Csi('J'),
            Event::EraseInDisplay(3),
            Event::EraseInDisplay(2),
            Event::Csi('J'),
        ]
    );
}

#[test]
fn erase_in_line_is_not_reported() {
    assert_eq!(events(&[b"\x1b[K\x1b[2K"]), []);
}

#[test]
fn reset_fires_after_the_screen_is_reset() {
    assert_eq!(
        events(&[b"hello\x1bc"]),
        [Event::Reset {
            contents: String::new()
        }]
    );
}

#[test]
fn alternate_screen_reports_entering_clearing_and_leaving() {
    assert_eq!(
        events(&[b"\x1b[?1049h", b"\x1b[?1049l", b"\x1b[?47h", b"\x1b[?47l"]),
        [
            Event::AlternateScreen {
                entered: true,
                cleared: true,
                active: true,
            },
            Event::AlternateScreen {
                entered: false,
                cleared: false,
                active: false,
            },
            Event::AlternateScreen {
                entered: true,
                cleared: false,
                active: true,
            },
            Event::AlternateScreen {
                entered: false,
                cleared: false,
                active: false,
            },
        ]
    );
}

#[test]
fn other_private_modes_do_not_report_the_alternate_screen() {
    assert_eq!(
        events(&[b"\x1b[?25l\x1b[?2004h\x1b[?1047h"]),
        [Event::Csi('h')]
    );
}

#[test]
fn a_sixel_reaches_the_dcs_callbacks_byte_for_byte() {
    assert_eq!(events(&[SIXEL]), sixel_events());
}

#[test]
fn a_sixel_split_across_reads_reaches_the_dcs_callbacks_unchanged() {
    for split in 0..=SIXEL.len() {
        let (head, tail) = SIXEL.split_at(split);
        assert_eq!(events(&[head, tail]), sixel_events(), "split at {split}");
    }
    let bytes: Vec<&[u8]> = SIXEL.chunks(1).collect();
    assert_eq!(events(&bytes), sixel_events());
}

#[test]
fn dcs_intermediates_and_cancel_are_reported() {
    assert_eq!(
        events(&[b"\x1bP$qm\x18x"]),
        [
            Event::Hook {
                params: vec![vec![0]],
                intermediates: vec![b'$'],
                ignore: false,
                action: 'q',
            },
            Event::Put(b'm'),
            Event::Unhook,
        ]
    );
}

#[test]
fn parts_mut_lends_the_screen_and_the_callbacks_together() {
    let mut parser = parser();
    let (screen, callbacks) = parser.parts_mut();
    screen.set_cursor_col(3);
    callbacks.0.push(Event::Unhook);
    assert_eq!(parser.screen().cursor_position(), (0, 3));
    assert_eq!(parser.callbacks().0, [Event::Unhook]);
}
