use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use super::connection::Origin;
use super::graphics::store::{ImageData, ImageStore};
use super::render::{ImageUse, Viewer};
use crate::protocol::ImageOp;

const LOCAL_CHUNK_LEN: usize = 1024 * 1024;
const PEER_CHUNK_LEN: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Turn {
    Frame,
    Upload,
}

pub struct Uploader {
    store: Arc<ImageStore>,
    chunk_len: usize,
    budget: u64,
    graphics: bool,
    clock: u64,
    held: BTreeMap<u32, Held>,
    used: u64,
    current: BTreeSet<u32>,
    refused: BTreeSet<u32>,
    queue: VecDeque<ImageUse>,
    deletes: VecDeque<u32>,
    upload: Option<Upload>,
    upload_next: bool,
}

struct Held {
    decoded: u64,
    used: u64,
    stale: bool,
}

enum Upload {
    Transmit {
        shown: ImageUse,
        data: ImageData,
        offset: usize,
        started: bool,
    },
    Place(ImageUse),
}

impl Upload {
    fn key(&self) -> u32 {
        match self {
            Self::Transmit { shown, .. } | Self::Place(shown) => shown.key,
        }
    }
}

impl Uploader {
    pub fn new(store: Arc<ImageStore>, origin: Origin, budget: u64) -> Self {
        Self {
            store,
            chunk_len: match origin {
                Origin::Local => LOCAL_CHUNK_LEN,
                Origin::Peer => PEER_CHUNK_LEN,
            },
            budget,
            graphics: false,
            clock: 0,
            held: BTreeMap::new(),
            used: 0,
            current: BTreeSet::new(),
            refused: BTreeSet::new(),
            queue: VecDeque::new(),
            deletes: VecDeque::new(),
            upload: None,
            upload_next: false,
        }
    }

    pub fn viewer(&self) -> Viewer<'_> {
        Viewer {
            graphics: self.graphics,
            hidden: &self.refused,
        }
    }

    pub fn set_budget(&mut self, budget: u64) {
        if budget > self.budget {
            self.refused.clear();
        }
        self.budget = budget;
    }

    pub fn set_graphics(&mut self, graphics: bool) {
        if self.graphics == graphics {
            return;
        }
        self.graphics = graphics;
        if graphics {
            return;
        }
        if let Some(Upload::Transmit {
            shown,
            data,
            started: true,
            ..
        }) = self.upload.take()
        {
            self.upload = Some(Upload::Transmit {
                shown,
                offset: data.bytes.len(),
                data,
                started: true,
            });
        }
        self.deletes.extend(self.held.keys());
        self.held.clear();
        self.used = 0;
        self.current.clear();
        self.refused.clear();
        self.queue.clear();
    }

    pub fn restart(&mut self) {
        let sending = self.upload.as_ref().map(Upload::key);
        for (key, held) in &mut self.held {
            held.stale = Some(*key) != sending;
        }
        self.refused.clear();
        self.queue.clear();
    }

    pub fn frame(&mut self, images: &[ImageUse]) {
        self.clock += 1;
        self.forget_dead();
        self.current = images.iter().map(|shown| shown.key).collect();
        for shown in images {
            let fresh = match self.held.get_mut(&shown.key) {
                Some(held) => {
                    held.used = self.clock;
                    !held.stale
                }
                None => false,
            };
            let waiting = self.queue.iter().any(|queued| queued.key == shown.key)
                || self.upload.as_ref().map(Upload::key) == Some(shown.key);
            if !fresh && !waiting {
                self.queue.push_back(*shown);
            }
        }
    }

    pub fn has_work(&self) -> bool {
        !self.deletes.is_empty() || self.upload.is_some() || !self.queue.is_empty()
    }

    pub fn choose(&mut self, frame_due: bool) -> Option<Turn> {
        let turn = match (frame_due, self.has_work()) {
            (true, true) if self.upload_next => Turn::Upload,
            (true, _) => Turn::Frame,
            (false, true) => Turn::Upload,
            (false, false) => return None,
        };
        self.upload_next = turn == Turn::Frame;
        Some(turn)
    }

    pub fn next(&mut self) -> Option<ImageOp> {
        if self.upload.is_none() {
            self.start();
        }
        if let Some(key) = self.deletes.pop_front() {
            return Some(ImageOp::Delete { key });
        }
        match self.upload.take()? {
            Upload::Transmit {
                shown,
                data,
                offset,
                ..
            } => {
                let end = data.bytes.len().min(offset + self.chunk_len);
                let last = end == data.bytes.len();
                let op = ImageOp::Transmit {
                    key: shown.key,
                    format: data.format,
                    width: data.width,
                    height: data.height,
                    compressed: data.compressed,
                    total: u32::try_from(data.bytes.len()).unwrap_or(u32::MAX),
                    data: data.bytes[offset..end].to_vec(),
                    last,
                };
                self.upload = if !last {
                    Some(Upload::Transmit {
                        shown,
                        data,
                        offset: end,
                        started: true,
                    })
                } else if self.graphics {
                    Some(Upload::Place(shown))
                } else {
                    None
                };
                Some(op)
            }
            Upload::Place(shown) => Some(ImageOp::Place {
                key: shown.key,
                cols: shown.cols,
                rows: shown.rows,
            }),
        }
    }

    fn start(&mut self) {
        while let Some(shown) = self.queue.pop_front() {
            let stale = match self.held.get(&shown.key) {
                Some(held) if !held.stale => continue,
                held => held.is_some(),
            };
            if !self.current.contains(&shown.key) || !self.store.is_displayed(shown.key) {
                continue;
            }
            let Some(data) = self.store.get(shown.image) else {
                continue;
            };
            let decoded = u64::try_from(data.decoded_len).unwrap_or(u64::MAX);
            if stale {
                if let Some(held) = self.held.get_mut(&shown.key) {
                    held.stale = false;
                }
            } else if self.make_room(decoded) {
                self.used += decoded;
                self.held.insert(
                    shown.key,
                    Held {
                        decoded,
                        used: self.clock,
                        stale: false,
                    },
                );
            } else {
                self.refused.insert(shown.key);
                continue;
            }
            self.upload = Some(Upload::Transmit {
                shown,
                data,
                offset: 0,
                started: false,
            });
            return;
        }
    }

    fn make_room(&mut self, decoded: u64) -> bool {
        let evictable: u64 = self
            .held
            .iter()
            .filter(|(key, _)| !self.current.contains(key))
            .map(|(_, held)| held.decoded)
            .sum();
        if (self.used - evictable).saturating_add(decoded) > self.budget {
            return false;
        }
        while self.used.saturating_add(decoded) > self.budget {
            let victim = self
                .held
                .iter()
                .filter(|(key, _)| !self.current.contains(key))
                .min_by_key(|(_, held)| held.used)
                .map(|(key, _)| *key);
            let Some(victim) = victim else {
                return false;
            };
            self.drop_held(victim);
        }
        true
    }

    fn forget_dead(&mut self) {
        let sending = self.upload.as_ref().map(Upload::key);
        let dead: Vec<u32> = self
            .held
            .keys()
            .copied()
            .filter(|key| Some(*key) != sending && !self.store.is_displayed(*key))
            .collect();
        if dead.is_empty() {
            return;
        }
        for key in dead {
            self.drop_held(key);
        }
        self.refused.clear();
    }

    fn drop_held(&mut self, key: u32) {
        if let Some(held) = self.held.remove(&key) {
            self.used -= held.decoded;
            self.deletes.push_back(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ImageFormat;
    use crate::server::graphics::store::{Buffer, PaneImages};
    use crate::server::graphics::transmit::Image;

    const MIB: usize = 1024 * 1024;

    struct Images {
        store: Arc<ImageStore>,
        pane: PaneImages,
        next_id: u32,
    }

    impl Images {
        fn new() -> Self {
            let store = Arc::new(ImageStore::new(1 << 30));
            let pane = store.open_pane();
            Self {
                store,
                pane,
                next_id: 1,
            }
        }

        fn uploader(&self, origin: Origin, budget: u64) -> Uploader {
            let mut uploader = Uploader::new(Arc::clone(&self.store), origin, budget);
            uploader.set_graphics(true);
            uploader
        }

        fn image(&mut self, len: usize, decoded_len: usize) -> ImageUse {
            let id = self.next_id;
            self.next_id += 1;
            let image = Image {
                width: 4,
                height: 2,
                format: ImageFormat::Png,
                compressed: false,
                bytes: (0..len).map(|at| (at % 251) as u8).collect(),
                decoded_len,
            };
            let stored = self.pane.insert(Buffer::Main, id, 0, image).unwrap();
            ImageUse {
                key: self.pane.mint_display().unwrap(),
                image: stored.key,
                cols: 3,
                rows: 2,
            }
        }

        fn bytes(&self, shown: ImageUse) -> Arc<[u8]> {
            self.store.get(shown.image).unwrap().bytes
        }
    }

    fn drain(uploader: &mut Uploader) -> Vec<ImageOp> {
        std::iter::from_fn(|| uploader.next()).collect()
    }

    fn summary(ops: &[ImageOp]) -> Vec<(char, u32, usize, bool)> {
        ops.iter()
            .map(|op| match op {
                ImageOp::Transmit {
                    key, data, last, ..
                } => ('t', *key, data.len(), *last),
                ImageOp::Place { key, .. } => ('p', *key, 0, true),
                ImageOp::Delete { key } => ('d', *key, 0, true),
            })
            .collect()
    }

    fn uploaded(ops: &[ImageOp], wanted: u32) -> Vec<u8> {
        ops.iter()
            .filter_map(|op| match op {
                ImageOp::Transmit { key, data, .. } if *key == wanted => Some(data.clone()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    #[test]
    fn a_local_client_gets_mebibyte_chunks_and_then_the_placement() {
        let mut images = Images::new();
        let shown = images.image(2 * MIB + MIB / 2, 100);
        let mut uploader = images.uploader(Origin::Local, 1 << 30);
        assert!(!uploader.has_work());
        uploader.frame(&[shown]);
        assert!(uploader.has_work());

        let ops = drain(&mut uploader);
        let key = shown.key;
        assert_eq!(
            summary(&ops),
            [
                ('t', key, MIB, false),
                ('t', key, MIB, false),
                ('t', key, MIB / 2, true),
                ('p', key, 0, true),
            ]
        );
        assert_eq!(uploaded(&ops, key), *images.bytes(shown));
        let ImageOp::Transmit {
            format,
            width,
            height,
            compressed,
            total,
            ..
        } = &ops[0]
        else {
            panic!("expected a transmission, got {:?}", ops[0]);
        };
        assert_eq!(
            (*format, *width, *height, *compressed, *total),
            (ImageFormat::Png, 4, 2, false, 2_621_440)
        );
        assert_eq!(
            ops[3],
            ImageOp::Place {
                key,
                cols: 3,
                rows: 2
            }
        );
        assert!(!uploader.has_work());
        uploader.frame(&[shown]);
        assert!(drain(&mut uploader).is_empty());
    }

    #[test]
    fn a_peer_gets_64_kib_chunks() {
        let mut images = Images::new();
        let shown = images.image(150 * 1024, 100);
        let mut uploader = images.uploader(Origin::Peer, 1 << 30);
        uploader.frame(&[shown]);
        let ops = drain(&mut uploader);
        let key = shown.key;
        assert_eq!(
            summary(&ops),
            [
                ('t', key, 64 * 1024, false),
                ('t', key, 64 * 1024, false),
                ('t', key, 22 * 1024, true),
                ('p', key, 0, true),
            ]
        );
        assert_eq!(uploaded(&ops, key), *images.bytes(shown));
    }

    #[test]
    fn a_full_budget_deletes_unused_images_least_recently_used_first() {
        let mut images = Images::new();
        let [first, second, third, fourth] = [(); 4].map(|()| images.image(8, 4));
        let mut uploader = images.uploader(Origin::Local, 10);
        uploader.frame(&[first]);
        drain(&mut uploader);
        uploader.frame(&[second]);
        drain(&mut uploader);
        uploader.frame(&[first]);
        assert!(drain(&mut uploader).is_empty());

        uploader.frame(&[third]);
        assert_eq!(
            summary(&drain(&mut uploader)),
            [
                ('d', second.key, 0, true),
                ('t', third.key, 8, true),
                ('p', third.key, 0, true),
            ]
        );
        uploader.frame(&[third, fourth]);
        assert_eq!(
            summary(&drain(&mut uploader)),
            [
                ('d', first.key, 0, true),
                ('t', fourth.key, 8, true),
                ('p', fourth.key, 0, true),
            ]
        );
    }

    #[test]
    fn an_image_that_cannot_fit_is_refused_and_hidden_from_compose() {
        let mut images = Images::new();
        let small = images.image(8, 6);
        let huge = images.image(8, 11);
        let other = images.image(8, 6);
        let mut uploader = images.uploader(Origin::Local, 10);
        uploader.frame(&[huge]);
        assert!(drain(&mut uploader).is_empty());
        assert!(uploader.viewer().hidden.contains(&huge.key));
        uploader.frame(&[]);
        assert!(drain(&mut uploader).is_empty());

        uploader.frame(&[small]);
        drain(&mut uploader);
        uploader.frame(&[small, other]);
        assert!(drain(&mut uploader).is_empty());
        assert!(uploader.viewer().hidden.contains(&other.key));
        assert!(!uploader.viewer().hidden.contains(&small.key));

        uploader.set_budget(20);
        assert!(uploader.viewer().hidden.is_empty());
        uploader.frame(&[small, other]);
        assert_eq!(
            summary(&drain(&mut uploader)),
            [('t', other.key, 8, true), ('p', other.key, 0, true)]
        );
    }

    #[test]
    fn a_dead_placement_is_deleted_from_the_client() {
        let mut images = Images::new();
        let shown = images.image(8, 4);
        let kept = images.image(8, 4);
        let mut uploader = images.uploader(Origin::Local, 100);
        uploader.frame(&[shown, kept]);
        drain(&mut uploader);

        uploader.frame(&[]);
        assert!(drain(&mut uploader).is_empty());
        images.pane.release_display(shown.key);
        uploader.frame(&[]);
        assert_eq!(drain(&mut uploader), [ImageOp::Delete { key: shown.key }]);
        uploader.frame(&[]);
        assert!(drain(&mut uploader).is_empty());
    }

    #[test]
    fn an_image_the_latest_frame_dropped_is_not_uploaded() {
        let mut images = Images::new();
        let gone = images.image(8, 4);
        let shown = images.image(8, 4);
        let mut uploader = images.uploader(Origin::Local, 100);
        uploader.frame(&[gone]);
        uploader.frame(&[shown]);
        assert_eq!(
            summary(&drain(&mut uploader)),
            [('t', shown.key, 8, true), ('p', shown.key, 0, true)]
        );
    }

    #[test]
    fn a_dead_placement_is_not_uploaded() {
        let mut images = Images::new();
        let shown = images.image(8, 4);
        let mut uploader = images.uploader(Origin::Local, 100);
        uploader.frame(&[shown]);
        images.pane.release_display(shown.key);
        assert!(drain(&mut uploader).is_empty());
        assert!(!uploader.has_work());
    }

    #[test]
    fn a_redraw_uploads_what_the_next_frame_shows_again() {
        let mut images = Images::new();
        let shown = images.image(8, 4);
        let elsewhere = images.image(8, 4);
        let mut uploader = images.uploader(Origin::Local, 100);
        uploader.frame(&[shown, elsewhere]);
        drain(&mut uploader);

        uploader.restart();
        uploader.frame(&[shown]);
        assert_eq!(
            summary(&drain(&mut uploader)),
            [('t', shown.key, 8, true), ('p', shown.key, 0, true)]
        );
        uploader.frame(&[shown]);
        assert!(drain(&mut uploader).is_empty());

        images.pane.release_display(elsewhere.key);
        uploader.frame(&[shown]);
        assert_eq!(
            drain(&mut uploader),
            [ImageOp::Delete { key: elsewhere.key }]
        );
    }

    #[test]
    fn a_redraw_lets_the_open_transmission_finish_once() {
        let mut images = Images::new();
        let shown = images.image(2 * MIB, 4);
        let mut uploader = images.uploader(Origin::Local, 100);
        uploader.frame(&[shown]);
        assert_eq!(
            summary(&[uploader.next().unwrap()]),
            [('t', shown.key, MIB, false)]
        );
        uploader.restart();
        uploader.frame(&[shown]);
        assert_eq!(
            summary(&drain(&mut uploader)),
            [('t', shown.key, MIB, true), ('p', shown.key, 0, true)]
        );
    }

    #[test]
    fn turning_graphics_off_ends_the_transmission_and_deletes_every_image() {
        let mut images = Images::new();
        let held = images.image(8, 4);
        let sending = images.image(2 * MIB, 4);
        let mut uploader = images.uploader(Origin::Local, 100);
        uploader.frame(&[held]);
        drain(&mut uploader);
        uploader.frame(&[held, sending]);
        assert_eq!(
            summary(&[uploader.next().unwrap()]),
            [('t', sending.key, MIB, false)]
        );

        uploader.set_graphics(false);
        assert!(!uploader.viewer().graphics);
        assert_eq!(
            summary(&drain(&mut uploader)),
            [
                ('d', held.key, 0, true),
                ('d', sending.key, 0, true),
                ('t', sending.key, 0, true),
            ]
        );
        uploader.frame(&[]);
        assert!(!uploader.has_work());

        uploader.set_graphics(true);
        uploader.frame(&[held]);
        assert_eq!(
            summary(&drain(&mut uploader)),
            [('t', held.key, 8, true), ('p', held.key, 0, true)]
        );
    }

    #[test]
    fn frames_and_uploads_take_turns_under_constant_output() {
        let mut images = Images::new();
        let shown = images.image(4 * MIB, 4);
        let mut uploader = images.uploader(Origin::Local, 100);
        assert_eq!(uploader.choose(false), None);
        assert_eq!(uploader.choose(true), Some(Turn::Frame));
        uploader.frame(&[shown]);

        let mut turns = Vec::new();
        while let Some(turn) = uploader.choose(true) {
            match turn {
                Turn::Frame => uploader.frame(&[shown]),
                Turn::Upload => {
                    uploader.next();
                }
            }
            turns.push(turn);
            if turns.len() == 12 {
                break;
            }
        }
        let alternating: Vec<Turn> = [Turn::Upload, Turn::Frame]
            .into_iter()
            .cycle()
            .take(10)
            .chain([Turn::Frame, Turn::Frame])
            .collect();
        assert_eq!(turns, alternating);
    }

    #[test]
    fn uploads_keep_going_while_no_frame_is_due() {
        let mut images = Images::new();
        let shown = images.image(3 * MIB, 4);
        let mut uploader = images.uploader(Origin::Local, 100);
        uploader.frame(&[shown]);
        let mut sent = 0;
        while let Some(turn) = uploader.choose(false) {
            assert_eq!(turn, Turn::Upload);
            uploader.next();
            sent += 1;
        }
        assert_eq!(sent, 4);
        assert_eq!(uploader.choose(true), Some(Turn::Frame));
    }
}
