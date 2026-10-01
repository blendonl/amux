pub mod apc;
pub mod command;
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
use respond::{Failure, Recipient};
use store::{Buffer, ImageKey, Name, PaneImages};
use transmit::{Step, Transmission, Transmissions};

type PaneParser = vt100::Parser<PaneCallbacks>;

pub struct PaneGraphics {
    scanner: ApcScanner,
    kitty: KittyGraphics,
}

impl PaneGraphics {
    pub fn new(images: PaneImages, callbacks: PaneCallbacks) -> Self {
        Self {
            scanner: ApcScanner::new(),
            kitty: KittyGraphics::new(images, callbacks),
        }
    }

    pub fn process(&mut self, output: &[u8], parser: &Mutex<PaneParser>) {
        for segment in self.scanner.split(output) {
            match segment {
                Segment::Text(text) => lock(parser).process(text),
                Segment::Graphics(body) => self.kitty.handle(&body, parser),
            }
        }
    }
}

struct KittyGraphics {
    images: PaneImages,
    transmissions: Transmissions,
    callbacks: PaneCallbacks,
}

impl KittyGraphics {
    fn new(images: PaneImages, callbacks: PaneCallbacks) -> Self {
        Self {
            images,
            transmissions: Transmissions::default(),
            callbacks,
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
        let buffer = if lock(parser).screen().alternate_screen() {
            Buffer::Alt
        } else {
            Buffer::Main
        };
        match command.action {
            Action::Transmit | Action::TransmitAndPlace | Action::Query => {
                self.transmit(buffer, command, parser);
            }
            Action::Delete => self.delete(buffer, &command),
            Action::Place | Action::Frame | Action::Animate | Action::Compose => {}
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
        if command.action == Action::Query && !self.callbacks.graphics() {
            return;
        }
        let recipient = Recipient {
            id: *outcome.as_ref().unwrap_or(&command.id),
            ..Recipient::of(&command)
        };
        if let Some(reply) = recipient.reply(&outcome.map(drop)) {
            self.callbacks.reply(reply);
        }
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
        if command.action == Action::TransmitAndPlace {
            self.place(stored.key, command, parser)?;
        }
        Ok(stored.id)
    }

    fn place(
        &mut self,
        _key: ImageKey,
        _command: &Command,
        _parser: &Mutex<PaneParser>,
    ) -> Result<(), Failure> {
        Ok(())
    }

    fn delete(&mut self, buffer: Buffer, command: &Command) {
        let name = match (command.delete, command.placement) {
            (b'I', 0) if command.id != 0 => Name::Id(command.id),
            (b'N', 0) if command.number != 0 => Name::Number(command.number),
            _ => return,
        };
        self.images.free(buffer, name);
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
        graphics: KittyGraphics,
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
            let callbacks = PaneCallbacks::new(input);
            let store = Arc::new(ImageStore::new(quota));
            let images = store.open_pane();
            Self {
                graphics: KittyGraphics::new(images.clone(), callbacks.clone()),
                parser: Mutex::new(vt100::Parser::new_with_callbacks(4, 10, 0, callbacks)),
                replies,
                store,
                images,
            }
        }

        fn send(&mut self, body: &str) -> Vec<String> {
            self.graphics.handle(body.as_bytes(), &self.parser);
            self.replies
                .try_iter()
                .map(|reply| String::from_utf8(reply).unwrap())
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
            "a=p,i=1",
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
