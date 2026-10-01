pub mod apc;
pub mod command;
pub mod place;
pub mod respond;
#[cfg_attr(not(test), expect(dead_code))]
pub mod sixel;
#[cfg_attr(not(test), expect(dead_code))]
pub mod sixel_palette;
pub mod store;
pub mod transmit;

use std::sync::{Mutex, MutexGuard, PoisonError};

use tracing::debug;

use super::replies::PaneCallbacks;
use apc::{ApcScanner, Segment};
use command::{Action, Command};
use place::active_buffer;
use respond::{Code, Failure, Recipient};
use store::{Buffer, Name, PaneImages, Stored};
use transmit::{Step, Transmission, Transmissions};

type PaneParser = vt100::Parser<PaneCallbacks>;

pub struct PaneGraphics {
    scanner: ApcScanner,
    kitty: KittyGraphics,
}

impl PaneGraphics {
    pub fn new(images: PaneImages) -> Self {
        Self {
            scanner: ApcScanner::new(),
            kitty: KittyGraphics::new(images),
        }
    }

    pub fn process(&mut self, output: &[u8], parser: &Mutex<PaneParser>) {
        for segment in self.scanner.split(output) {
            match segment {
                Segment::Text(text) => lock(parser).process(text),
                Segment::Unwrapped(text) => lock(parser).process(&text),
                Segment::Graphics(body) => self.kitty.handle(&body, parser),
            }
        }
        settle(parser);
    }
}

fn settle(parser: &Mutex<PaneParser>) {
    let mut parser = lock(parser);
    let (screen, callbacks) = parser.parts_mut();
    if let Some(placements) = callbacks.placements_mut() {
        placements.settle(screen);
    }
}

struct KittyGraphics {
    images: PaneImages,
    transmissions: Transmissions,
}

impl KittyGraphics {
    fn new(images: PaneImages) -> Self {
        Self {
            images,
            transmissions: Transmissions::default(),
        }
    }

    fn handle(&mut self, body: &[u8], parser: &Mutex<PaneParser>) {
        let command = match Command::parse(body) {
            Ok(command) => command,
            Err(error) => {
                debug!(?error, "ignoring a malformed kitty graphics command");
                return;
            }
        };
        let buffer = active_buffer(lock(parser).screen());
        match command.action {
            Action::Transmit | Action::TransmitAndPlace | Action::Query => {
                self.transmit(buffer, command, parser);
            }
            Action::Place => self.put(buffer, &command, parser),
            Action::Delete => self.delete(buffer, &command, parser),
            Action::Frame | Action::Animate | Action::Compose => {}
        }
    }

    fn transmit(&mut self, buffer: Buffer, command: Command, parser: &Mutex<PaneParser>) {
        let (command, outcome) = match self.transmissions.receive(buffer, command) {
            Step::Waiting | Step::Swallowed => return,
            Step::Failed(command, failure) => (command, Err(failure)),
            Step::Complete(Transmission { command, data }) => {
                let outcome = self.complete(buffer, &command, data, parser);
                (command, outcome)
            }
        };
        if command.action == Action::Query && !lock(parser).callbacks().graphics() {
            return;
        }
        answer(&command, outcome, parser);
    }

    fn put(&mut self, buffer: Buffer, command: &Command, parser: &Mutex<PaneParser>) {
        let name = match (command.id, command.number) {
            (0, 0) => return,
            (0, number) => Name::Number(number),
            (id, _) => Name::Id(id),
        };
        let outcome = if command.id != 0 && command.number != 0 {
            Err(Failure::new(
                Code::Einval,
                "an image can't have both an id and a number",
            ))
        } else {
            self.images
                .find(buffer, name)
                .ok_or_else(|| Failure::new(Code::Enoent, "no such image"))
                .and_then(|image| {
                    place(image, command, parser)?;
                    Ok(image.id)
                })
        };
        answer(command, outcome, parser);
    }

    fn complete(
        &mut self,
        buffer: Buffer,
        command: &Command,
        data: Vec<u8>,
        parser: &Mutex<PaneParser>,
    ) -> Result<u32, Failure> {
        let image = transmit::load(command, data)?;
        if command.action == Action::Query {
            return Ok(command.id);
        }
        let stored = self
            .images
            .insert(buffer, command.id, command.number, image.packed())?;
        if let Some(placements) = lock(parser).callbacks_mut().placements_mut() {
            placements.forget_replaced(buffer, stored);
        }
        if command.action == Action::TransmitAndPlace {
            place(stored, command, parser)?;
        }
        Ok(stored.id)
    }

    fn delete(&mut self, buffer: Buffer, command: &Command, parser: &Mutex<PaneParser>) {
        self.transmissions.abort(buffer);
        {
            let mut parser = lock(parser);
            let (screen, callbacks) = parser.parts_mut();
            if let Some(placements) = callbacks.placements_mut() {
                placements.delete(screen, command);
            }
        }
        match (command.delete, command.placement) {
            (b'I', 0) if command.id != 0 => {
                self.images.free(buffer, Name::Id(command.id));
            }
            (b'N', 0) if command.number != 0 => {
                self.images.free(buffer, Name::Number(command.number));
            }
            (b'R', 0) => self
                .images
                .free_ids(buffer, command.source_x..=command.source_y),
            _ => {}
        }
    }
}

fn place(image: Stored, command: &Command, parser: &Mutex<PaneParser>) -> Result<(), Failure> {
    let mut parser = lock(parser);
    let (screen, callbacks) = parser.parts_mut();
    let cell_pixels = callbacks.cell_pixels();
    match callbacks.placements_mut() {
        Some(placements) => placements.place(screen, image, command, cell_pixels),
        None => Err(Failure::new(Code::Einval, "images are off in this pane")),
    }
}

fn answer(command: &Command, outcome: Result<u32, Failure>, parser: &Mutex<PaneParser>) {
    let recipient = Recipient {
        id: *outcome.as_ref().unwrap_or(&command.id),
        ..Recipient::of(command)
    };
    if let Some(reply) = recipient.reply(&outcome.map(drop)) {
        lock(parser).callbacks().reply(reply);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::sync::{mpsc, Arc};

    use super::store::ImageStore;
    use super::*;

    const PIXEL: &str = "f=24,s=1,v=1;AAAA";

    struct Pane {
        graphics: PaneGraphics,
        parser: Mutex<PaneParser>,
        replies: mpsc::Receiver<Vec<u8>>,
        store: Arc<ImageStore>,
        images: PaneImages,
    }

    impl Pane {
        fn new() -> Self {
            Self::with_quota(1 << 30)
        }

        fn with_quota(quota: u64) -> Self {
            let (input, replies) = mpsc::channel();
            let store = Arc::new(ImageStore::new(quota));
            let images = store.open_pane();
            let callbacks = PaneCallbacks::new(input, Some(images.clone()));
            Self {
                graphics: PaneGraphics::new(images.clone()),
                parser: Mutex::new(vt100::Parser::new_with_callbacks(4, 10, 0, callbacks)),
                replies,
                store,
                images,
            }
        }

        fn send(&mut self, body: &str) -> Vec<String> {
            self.output(&format!("\x1b_G{body}\x1b\\"))
        }

        fn output(&mut self, output: &str) -> Vec<String> {
            self.graphics.process(output.as_bytes(), &self.parser);
            self.replies
                .try_iter()
                .map(|reply| String::from_utf8(reply).unwrap())
                .collect()
        }

        fn cursor(&self) -> (u16, u16) {
            lock(&self.parser).screen().cursor_position()
        }

        fn spans(&self) -> Vec<(u16, u16, u16)> {
            let parser = lock(&self.parser);
            let placements = parser.callbacks().placements().unwrap();
            placements
                .spans(parser.screen())
                .map(|span| (span.row, span.col, span.cols))
                .collect()
        }

        fn show_images(&self, graphics: bool) {
            lock(&self.parser).callbacks().set_graphics(graphics);
        }

        fn image(&self, buffer: Buffer, name: Name) -> Option<store::ImageData> {
            let key = self.images.lookup(buffer, name)?;
            self.store.get(key)
        }
    }

    fn ok(keys: &str) -> String {
        format!("\x1b_G{keys};OK\x1b\\")
    }

    #[test]
    fn a_transmission_is_stored_and_answered() {
        let mut pane = Pane::new();
        assert_eq!(pane.send(&format!("a=t,i=1,{PIXEL}")), [ok("i=1")]);
        let stored = pane.image(Buffer::Main, Name::Id(1)).unwrap();
        assert_eq!((stored.width, stored.height, stored.decoded_len), (1, 1, 3));
        assert!(stored.compressed);
        assert_eq!(
            miniz_oxide::inflate::decompress_to_vec_zlib(&stored.bytes).unwrap(),
            [0, 0, 0]
        );

        assert_eq!(pane.send(&format!("a=T,i=2,p=7,{PIXEL}")), [ok("i=2,p=7")]);
        assert!(pane.image(Buffer::Main, Name::Id(2)).is_some());
    }

    #[test]
    fn an_error_is_answered_with_its_code() {
        let mut pane = Pane::new();
        assert_eq!(
            pane.send("a=t,i=3,f=24,s=2,v=1;AAAA"),
            ["\x1b_Gi=3;ENODATA:insufficient image data: 3 < 6\x1b\\"]
        );
        assert!(pane.image(Buffer::Main, Name::Id(3)).is_none());

        let mut small = Pane::with_quota(2);
        let replies = small.send(&format!("i=4,{PIXEL}"));
        assert_eq!(replies.len(), 1);
        assert!(replies[0].starts_with("\x1b_Gi=4;EFBIG:"), "{replies:?}");
    }

    #[test]
    fn quiet_levels_hide_ok_and_then_errors() {
        let mut pane = Pane::new();
        assert!(pane.send(&format!("i=1,q=1,{PIXEL}")).is_empty());
        assert!(pane.image(Buffer::Main, Name::Id(1)).is_some());
        assert_eq!(pane.send("i=2,q=1,f=24;AAAA").len(), 1);
        assert!(pane.send(&format!("i=3,q=2,{PIXEL}")).is_empty());
        assert!(pane.send("i=4,q=2,f=24;AAAA").is_empty());
    }

    #[test]
    fn nothing_is_answered_without_an_id_or_a_number() {
        let mut pane = Pane::new();
        assert!(pane.send(PIXEL).is_empty());
        assert!(pane.send("f=24;AAAA").is_empty());
        assert!(pane.send(&format!("p=3,{PIXEL}")).is_empty());
    }

    #[test]
    fn a_chunked_transmission_is_answered_once_at_its_end() {
        let mut pane = Pane::new();
        assert!(pane.send("i=5,f=24,s=2,v=1,m=1;AAA").is_empty());
        assert!(pane.send("m=1;AAA").is_empty());
        assert_eq!(pane.send("m=0;AA"), [ok("i=5")]);
        assert_eq!(pane.image(Buffer::Main, Name::Id(5)).unwrap().width, 2);

        assert!(pane.send("i=6,f=24,s=2,v=1,m=1;AAAA").is_empty());
        let failed = pane.send("m=1;!!!!");
        assert_eq!(failed.len(), 1);
        assert!(failed[0].starts_with("\x1b_Gi=6;EINVAL:"), "{failed:?}");
        assert!(pane.send("m=1;AAAA").is_empty());
        assert!(pane.send("m=0;AAAA").is_empty());
        assert!(pane.image(Buffer::Main, Name::Id(6)).is_none());

        assert!(pane.send("i=7,f=24,s=1,v=1,m=1;AA").is_empty());
        assert!(pane.send("q=1,m=0;AA").is_empty());
        assert!(pane.image(Buffer::Main, Name::Id(7)).is_some());
    }

    #[test]
    fn a_number_alone_is_answered_with_the_id_it_was_given() {
        let mut pane = Pane::new();
        assert_eq!(pane.send(&format!("I=9,{PIXEL}")), [ok("i=1,I=9")]);
        assert_eq!(pane.send(&format!("I=9,{PIXEL}")), [ok("i=2,I=9")]);
        assert_eq!(
            pane.image(Buffer::Main, Name::Number(9)).unwrap().key,
            pane.image(Buffer::Main, Name::Id(2)).unwrap().key
        );
        let both = pane.send(&format!("i=1,I=9,{PIXEL}"));
        assert_eq!(both.len(), 1);
        assert!(both[0].starts_with("\x1b_Gi=1,I=9;EINVAL:"), "{both:?}");
    }

    #[test]
    fn a_query_is_answered_only_while_the_client_shows_images() {
        let mut pane = Pane::new();
        let probe = "i=31,s=1,v=1,a=q,t=d,f=24;AAAA";
        assert!(pane.send(probe).is_empty());
        assert!(pane.send("i=32,a=q,f=24;AAAA").is_empty());

        pane.show_images(true);
        assert_eq!(pane.send(probe), [ok("i=31")]);
        let failed = pane.send("i=32,a=q,f=24;AAAA");
        assert_eq!(failed.len(), 1);
        assert!(failed[0].starts_with("\x1b_Gi=32;EINVAL:"), "{failed:?}");
        assert!(pane.send("i=33,a=q,q=1,f=24,s=1,v=1;AAAA").is_empty());
        assert!(pane.image(Buffer::Main, Name::Id(31)).is_none());

        pane.show_images(false);
        assert!(pane.send(probe).is_empty());
    }

    #[test]
    fn the_uppercase_deletes_free_images_by_id_or_number() {
        let mut pane = Pane::new();
        pane.send(&format!("i=1,{PIXEL}"));
        pane.send(&format!("I=5,{PIXEL}"));
        assert!(pane.send("a=d,d=i,i=1").is_empty());
        assert!(pane.image(Buffer::Main, Name::Id(1)).is_some());
        pane.send("a=d,d=I,i=1,p=2");
        assert!(pane.image(Buffer::Main, Name::Id(1)).is_some());
        pane.send("a=d,d=I,i=1");
        assert!(pane.image(Buffer::Main, Name::Id(1)).is_none());
        pane.send("a=d,d=n,I=5");
        assert!(pane.image(Buffer::Main, Name::Number(5)).is_some());
        pane.send("a=d,d=N,I=5");
        assert!(pane.image(Buffer::Main, Name::Number(5)).is_none());
        assert!(pane.image(Buffer::Main, Name::Id(2)).is_none());
    }

    #[test]
    fn a_put_is_answered_with_its_image_and_placement() {
        let mut pane = Pane::new();
        pane.send(&format!("i=1,q=2,{PIXEL}"));
        assert_eq!(pane.send("a=p,i=1,p=4"), [ok("i=1,p=4")]);
        assert!(pane.send("a=p,i=1,p=5,q=1").is_empty());
        assert!(pane.send("a=p,i=1,p=6,q=2").is_empty());
        let missing = pane.send("a=p,i=2,q=1");
        assert_eq!(missing.len(), 1);
        assert!(missing[0].starts_with("\x1b_Gi=2;ENOENT:"), "{missing:?}");
        assert!(pane.send("a=p,i=2,q=2").is_empty());
        assert!(pane.send("a=p,p=3").is_empty());
        assert_eq!(pane.spans(), [(0, 0, 1), (0, 1, 1), (0, 2, 1)]);
    }

    #[test]
    fn a_put_by_number_answers_with_the_id_of_the_newest_image() {
        let mut pane = Pane::new();
        pane.send(&format!("I=7,q=2,{PIXEL}"));
        pane.send(&format!("I=7,q=2,{PIXEL}"));
        assert_eq!(pane.send("a=p,I=7,p=1"), [ok("i=2,I=7,p=1")]);
        let missing = pane.send("a=p,I=8");
        assert_eq!(missing.len(), 1);
        assert!(missing[0].starts_with("\x1b_GI=8;ENOENT:"), "{missing:?}");
        let both = pane.send("a=p,i=1,I=7");
        assert_eq!(both.len(), 1);
        assert!(both[0].starts_with("\x1b_Gi=1,I=7;EINVAL:"), "{both:?}");
    }

    #[test]
    fn transmit_and_place_moves_the_cursor_before_the_text_after_it() {
        let mut pane = Pane::new();
        pane.output(&format!("ab\x1b_Ga=T,q=2,c=3,r=2,{PIXEL}\x1b\\cd"));
        assert_eq!(pane.spans(), [(0, 2, 3), (1, 2, 3)]);
        assert_eq!(pane.cursor(), (1, 7));
        let parser = lock(&pane.parser);
        assert_eq!(parser.screen().cell(1, 5).unwrap().contents(), "c");
    }

    #[test]
    fn a_failed_placement_is_answered_and_keeps_the_image() {
        let mut pane = Pane::new();
        let failed = pane.send(&format!("a=T,i=3,P=9,{PIXEL}"));
        assert_eq!(failed.len(), 1);
        assert!(failed[0].starts_with("\x1b_Gi=3;ENOPARENT:"), "{failed:?}");
        assert!(pane.image(Buffer::Main, Name::Id(3)).is_some());
        assert_eq!(pane.cursor(), (0, 0));
        assert!(pane.spans().is_empty());
    }

    #[test]
    fn sending_an_id_again_drops_the_placements_of_the_old_image() {
        let mut pane = Pane::new();
        pane.send(&format!("a=T,i=1,q=2,{PIXEL}"));
        pane.send(&format!("a=T,i=2,q=2,{PIXEL}"));
        assert_eq!(pane.spans().len(), 2);
        pane.send(&format!("a=t,i=1,q=2,{PIXEL}"));
        assert_eq!(pane.spans(), [(0, 1, 1)]);
    }

    #[test]
    fn a_delete_aborts_a_chunked_upload() {
        let mut pane = Pane::new();
        pane.show_images(true);
        assert!(pane.send("i=5,f=24,s=2,v=1,m=1;AAA").is_empty());
        assert!(pane.send("a=d,d=i,i=9").is_empty());
        assert!(pane.send("m=0;AAA").is_empty());
        assert!(pane.image(Buffer::Main, Name::Id(5)).is_none());
    }

    #[test]
    fn the_alternate_screen_has_its_own_ids() {
        let mut pane = Pane::new();
        pane.send(&format!("i=1,{PIXEL}"));
        lock(&pane.parser).process(b"\x1b[?1049h");
        assert!(pane.send("i=1,q=2,f=32,s=1,v=1;AAAAAA==").is_empty());
        let alt = pane.image(Buffer::Alt, Name::Id(1)).unwrap();
        let main = pane.image(Buffer::Main, Name::Id(1)).unwrap();
        assert_ne!(alt.key, main.key);
        assert_eq!((alt.decoded_len, main.decoded_len), (4, 3));
        pane.send("a=d,d=I,i=1");
        assert!(pane.image(Buffer::Alt, Name::Id(1)).is_none());
        assert!(pane.image(Buffer::Main, Name::Id(1)).is_some());
    }

    #[test]
    fn malformed_and_later_phase_commands_are_ignored() {
        let mut pane = Pane::new();
        pane.show_images(true);
        for body in [
            "i=x;AAAA",
            "a=z,i=1",
            "a=f,i=1;AAAA",
            "a=a,i=1",
            "a=c,i=1",
            "a=d,d=a",
            "a=d,d=z",
        ] {
            assert!(pane.send(body).is_empty(), "{body}");
        }
        assert!(pane.image(Buffer::Main, Name::Id(1)).is_none());
    }
}
