use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use tracing::debug;

use super::connection::Origin;
use super::graphics::derive::{self, Plan};
use super::graphics::place::ASSUMED_CELL_PIXELS;
use super::graphics::store::{Derived, ImageData, ImageStore};
use super::render::{ImageUse, Viewer};
use crate::protocol::{CellPixels, ImageOp};

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
    cell: CellPixels,
    checked: CellPixels,
    clock: u64,
    held: BTreeMap<u32, Held>,
    used: u64,
    current: BTreeSet<u32>,
    refused: BTreeSet<u32>,
    queue: VecDeque<ImageUse>,
    deletes: VecDeque<u32>,
    upload: Option<Upload>,
    upload_next: bool,
    deriving: BTreeMap<u32, ImageUse>,
    jobs: VecDeque<Derivation>,
    running: Option<u32>,
    ready: BTreeMap<u32, Derived>,
}

struct Held {
    shown: ImageUse,
    decoded: u64,
    used: u64,
    stale: bool,
    made_for: Option<CellPixels>,
}

pub struct Derivation {
    key: u32,
    cell: CellPixels,
    plan: Plan,
    source: ImageData,
    store: Arc<ImageStore>,
}

pub struct Finished {
    key: u32,
    cell: CellPixels,
    derived: Derived,
}

impl Derivation {
    pub fn run(self) -> Finished {
        let derived = match derive::derive(&self.source, &self.plan) {
            Ok(image) => Derived::Image(image),
            Err(error) => {
                debug!(
                    key = self.key,
                    "showing the image as sent instead of deriving it: {error:#}"
                );
                Derived::Plain
            }
        };
        Finished {
            key: self.key,
            cell: self.cell,
            derived: self.store.keep_derived(self.key, self.cell, derived),
        }
    }
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
            cell: ASSUMED_CELL_PIXELS,
            checked: ASSUMED_CELL_PIXELS,
            clock: 0,
            held: BTreeMap::new(),
            used: 0,
            current: BTreeSet::new(),
            refused: BTreeSet::new(),
            queue: VecDeque::new(),
            deletes: VecDeque::new(),
            upload: None,
            upload_next: false,
            deriving: BTreeMap::new(),
            jobs: VecDeque::new(),
            running: None,
            ready: BTreeMap::new(),
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

    pub fn set_cell_pixels(&mut self, pixels: Option<CellPixels>) {
        let cell = pixels.unwrap_or(ASSUMED_CELL_PIXELS);
        if cell == self.cell {
            return;
        }
        self.cell = cell;
        self.ready.clear();
        self.refused.clear();
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
        self.deriving.clear();
        self.jobs.clear();
        self.ready.clear();
        self.checked = self.cell;
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
        self.forget_outdated();
        self.current = images.iter().map(|shown| shown.key).collect();
        let current = &self.current;
        self.ready.retain(|key, _| current.contains(key));
        self.jobs.retain(|job| current.contains(&job.key));
        let running = self.running;
        self.deriving
            .retain(|key, _| current.contains(key) || Some(*key) == running);
        for shown in images {
            let fresh = match self.held.get_mut(&shown.key) {
                Some(held) => {
                    held.used = self.clock;
                    !held.stale
                }
                None => false,
            };
            let waiting = self.queue.iter().any(|queued| queued.key == shown.key)
                || self.upload.as_ref().map(Upload::key) == Some(shown.key)
                || self.deriving.contains_key(&shown.key);
            if !fresh && !waiting {
                self.queue.push_back(*shown);
            }
        }
    }

    pub fn has_work(&self) -> bool {
        !self.deletes.is_empty() || self.upload.is_some() || !self.queue.is_empty()
    }

    pub fn take_job(&mut self) -> Option<Derivation> {
        if self.running.is_some() {
            return None;
        }
        let job = self.jobs.pop_front()?;
        self.running = Some(job.key);
        Some(job)
    }

    pub fn finish(&mut self, finished: Finished) {
        self.running = None;
        let Some(shown) = self.deriving.remove(&finished.key) else {
            return;
        };
        if finished.cell == self.cell {
            self.ready.insert(finished.key, finished.derived);
        }
        if self.current.contains(&finished.key) {
            self.queue.push_back(shown);
        }
    }

    pub fn abandon(&mut self) {
        if let Some(key) = self.running {
            let cell = self.cell;
            self.finish(Finished {
                key,
                cell,
                derived: Derived::Plain,
            });
        }
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
            let Some(source) = self.store.get(shown.image) else {
                continue;
            };
            let plan = self.plan(&shown, &source);
            let data = match plan.filter(Plan::fits) {
                None => source,
                Some(plan) => match self
                    .ready
                    .remove(&shown.key)
                    .or_else(|| self.store.derived(shown.key, self.cell))
                {
                    Some(Derived::Image(derived)) => derived,
                    Some(Derived::Plain) => source,
                    None => {
                        self.request(shown, plan, source);
                        continue;
                    }
                },
            };
            let made_for = plan.map(|_| self.cell);
            let decoded = u64::try_from(data.decoded_len).unwrap_or(u64::MAX);
            if stale {
                if let Some(held) = self.held.get_mut(&shown.key) {
                    self.used = self.used - held.decoded + decoded;
                    *held = Held {
                        shown,
                        decoded,
                        used: held.used,
                        stale: false,
                        made_for,
                    };
                }
            } else if self.make_room(decoded) {
                self.used += decoded;
                self.held.insert(
                    shown.key,
                    Held {
                        shown,
                        decoded,
                        used: self.clock,
                        stale: false,
                        made_for,
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

    fn plan(&self, shown: &ImageUse, source: &ImageData) -> Option<Plan> {
        shown.look?.plan(
            (source.width, source.height),
            (shown.cols, shown.rows),
            self.cell,
        )
    }

    fn request(&mut self, shown: ImageUse, plan: Plan, source: ImageData) {
        self.deriving.insert(shown.key, shown);
        self.jobs.push_back(Derivation {
            key: shown.key,
            cell: self.cell,
            plan,
            source,
            store: Arc::clone(&self.store),
        });
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

    fn forget_outdated(&mut self) {
        if self.checked == self.cell {
            return;
        }
        let sending = self.upload.as_ref().map(Upload::key);
        let outdated: Vec<u32> = self
            .held
            .iter()
            .filter(|(_, held)| held.made_for != self.made_for(&held.shown))
            .map(|(key, _)| *key)
            .collect();
        if !sending.is_some_and(|key| outdated.contains(&key)) {
            self.checked = self.cell;
        }
        for key in outdated {
            if Some(key) != sending {
                self.drop_held(key);
            }
        }
    }

    fn made_for(&self, shown: &ImageUse) -> Option<CellPixels> {
        let source = self.store.get(shown.image)?;
        self.plan(shown, &source).map(|_| self.cell)
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
    use miniz_oxide::inflate::decompress_to_vec_zlib;

    use super::*;
    use crate::protocol::ImageFormat;
    use crate::server::graphics::derive::{Look, Sizing};
    use crate::server::graphics::place::{CellOffset, SourceRect};
    use crate::server::graphics::store::{Buffer, PaneImages};
    use crate::server::graphics::transmit::Image;

    const MIB: usize = 1024 * 1024;
    const SMALL: CellPixels = CellPixels {
        width: 2,
        height: 2,
    };
    const LARGE: CellPixels = CellPixels {
        width: 4,
        height: 4,
    };
    const CLEAR: [u8; 4] = [0; 4];

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
            self.insert(Image {
                width: 4,
                height: 2,
                format: ImageFormat::Png,
                compressed: false,
                bytes: (0..len).map(|at| (at % 251) as u8).collect(),
                decoded_len,
            })
        }

        fn pixels(&mut self, cells: (u16, u16), look: Option<Look>) -> ImageUse {
            let bytes: Vec<u8> = (0..8).flat_map(|at| [at, 0, 0, 255]).collect();
            let image = Image {
                width: 4,
                height: 2,
                format: ImageFormat::Rgba32,
                compressed: false,
                decoded_len: bytes.len(),
                bytes,
            };
            let (cols, rows) = cells;
            ImageUse {
                cols,
                rows,
                look,
                ..self.insert(image.packed())
            }
        }

        fn insert(&mut self, image: Image) -> ImageUse {
            let id = self.next_id;
            self.next_id += 1;
            let stored = self.pane.insert(Buffer::Main, id, 0, image).unwrap();
            ImageUse {
                key: self.pane.mint_display().unwrap(),
                image: stored.key,
                cols: 3,
                rows: 2,
                look: None,
            }
        }

        fn bytes(&self, shown: ImageUse) -> Arc<[u8]> {
            self.store.get(shown.image).unwrap().bytes
        }
    }

    fn drain(uploader: &mut Uploader) -> Vec<ImageOp> {
        std::iter::from_fn(|| uploader.next()).collect()
    }

    fn settle(uploader: &mut Uploader) -> Vec<ImageOp> {
        let mut ops = drain(uploader);
        while let Some(job) = uploader.take_job() {
            uploader.finish(job.run());
            ops.extend(drain(uploader));
        }
        ops
    }

    fn order(ops: &[ImageOp]) -> Vec<(char, u32)> {
        summary(ops)
            .into_iter()
            .map(|(op, key, _, _)| (op, key))
            .collect()
    }

    fn look(source: SourceRect, sizing: Sizing) -> Option<Look> {
        Some(Look {
            source,
            offset: CellOffset::default(),
            sizing,
        })
    }

    fn whole() -> SourceRect {
        SourceRect {
            x: 0,
            y: 0,
            width: 4,
            height: 2,
        }
    }

    fn right_half() -> SourceRect {
        SourceRect {
            x: 2,
            y: 0,
            width: 2,
            height: 2,
        }
    }

    fn pixel(at: u8) -> [u8; 4] {
        [at, 0, 0, 255]
    }

    fn derived(ops: &[ImageOp], wanted: u32) -> ((u32, u32), Vec<[u8; 4]>) {
        let size = ops
            .iter()
            .find_map(|op| match op {
                ImageOp::Transmit {
                    key,
                    format: ImageFormat::Rgba32,
                    width,
                    height,
                    compressed: true,
                    ..
                } if *key == wanted => Some((*width, *height)),
                _ => None,
            })
            .expect("a compressed RGBA transmission");
        let pixels = decompress_to_vec_zlib(&uploaded(ops, wanted))
            .unwrap()
            .chunks(4)
            .map(|pixel| pixel.try_into().unwrap())
            .collect();
        (size, pixels)
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
    fn a_look_the_terminal_already_shows_uploads_the_original_bytes() {
        let mut images = Images::new();
        let native = ImageUse {
            cols: 2,
            rows: 1,
            look: look(whole(), Sizing::Native),
            ..images.image(100, 32)
        };
        let stretched = ImageUse {
            cols: 4,
            rows: 2,
            look: look(whole(), Sizing::Stretch),
            ..images.image(100, 32)
        };
        let mut uploader = images.uploader(Origin::Local, 1 << 30);
        uploader.set_cell_pixels(Some(SMALL));
        uploader.frame(&[native, stretched]);
        let ops = drain(&mut uploader);
        assert!(uploader.take_job().is_none());
        assert_eq!(
            summary(&ops),
            [
                ('t', native.key, 100, true),
                ('p', native.key, 0, true),
                ('t', stretched.key, 100, true),
                ('p', stretched.key, 0, true),
            ]
        );
        assert_eq!(uploaded(&ops, native.key), *images.bytes(native));
        assert_eq!(uploaded(&ops, stretched.key), *images.bytes(stretched));
    }

    #[test]
    fn a_crop_is_derived_by_a_job_and_uploaded_once_it_is_ready() {
        let mut images = Images::new();
        let shown = images.pixels((1, 1), look(right_half(), Sizing::Native));
        let mut uploader = images.uploader(Origin::Local, 1 << 30);
        uploader.set_cell_pixels(Some(SMALL));
        uploader.frame(&[shown]);
        assert!(drain(&mut uploader).is_empty());
        assert!(!uploader.has_work());
        uploader.frame(&[shown]);
        let job = uploader.take_job().unwrap();
        assert!(uploader.take_job().is_none());
        assert!(drain(&mut uploader).is_empty());

        uploader.finish(job.run());
        let ops = drain(&mut uploader);
        assert_eq!(summary(&ops)[1..], [('p', shown.key, 0, true)], "{ops:?}");
        assert_eq!(
            ops[1],
            ImageOp::Place {
                key: shown.key,
                cols: 1,
                rows: 1
            }
        );
        assert_eq!(
            derived(&ops, shown.key),
            ((2, 2), vec![pixel(2), pixel(3), pixel(6), pixel(7)])
        );
        assert!(matches!(
            images.store.derived(shown.key, SMALL),
            Some(Derived::Image(_))
        ));
        uploader.frame(&[shown]);
        assert!(settle(&mut uploader).is_empty());

        let mut other = images.uploader(Origin::Peer, 1 << 30);
        other.set_cell_pixels(Some(SMALL));
        other.frame(&[shown]);
        assert_eq!(drain(&mut other), ops);
        assert!(other.take_job().is_none());
    }

    #[test]
    fn a_cell_size_change_derives_and_uploads_the_image_again() {
        let mut images = Images::new();
        let cropped = images.pixels((1, 1), look(right_half(), Sizing::Native));
        let exact = images.pixels((2, 1), look(whole(), Sizing::Native));
        let (c, e) = (cropped.key, exact.key);
        let mut uploader = images.uploader(Origin::Local, 1 << 30);
        uploader.set_cell_pixels(Some(SMALL));
        uploader.frame(&[cropped, exact]);
        let ops = settle(&mut uploader);
        assert_eq!(order(&ops), [('t', e), ('p', e), ('t', c), ('p', c)]);
        assert_eq!(uploaded(&ops, e), *images.bytes(exact));
        assert_eq!(derived(&ops, c).0, (2, 2));

        uploader.set_cell_pixels(Some(SMALL));
        uploader.frame(&[cropped, exact]);
        assert!(settle(&mut uploader).is_empty());

        uploader.set_cell_pixels(Some(LARGE));
        uploader.frame(&[cropped, exact]);
        let ops = settle(&mut uploader);
        assert_eq!(
            order(&ops),
            [('d', c), ('d', e), ('t', c), ('p', c), ('t', e), ('p', e)]
        );
        assert_eq!(
            derived(&ops, c),
            (
                (4, 4),
                [
                    [pixel(2), pixel(3), CLEAR, CLEAR],
                    [pixel(6), pixel(7), CLEAR, CLEAR],
                    [CLEAR; 4],
                    [CLEAR; 4],
                ]
                .concat()
            )
        );
        assert_eq!(
            derived(&ops, e),
            (
                (8, 4),
                [
                    [0, 1, 2, 3].map(pixel),
                    [CLEAR; 4],
                    [4, 5, 6, 7].map(pixel),
                    [CLEAR; 4],
                    [CLEAR; 4],
                    [CLEAR; 4],
                    [CLEAR; 4],
                    [CLEAR; 4],
                ]
                .concat()
            )
        );
        uploader.frame(&[cropped, exact]);
        assert!(settle(&mut uploader).is_empty());

        uploader.set_cell_pixels(Some(SMALL));
        uploader.frame(&[cropped, exact]);
        let ops = settle(&mut uploader);
        assert_eq!(
            order(&ops),
            [('d', c), ('d', e), ('t', e), ('p', e), ('t', c), ('p', c)]
        );
        assert_eq!(uploaded(&ops, e), *images.bytes(exact));
    }

    #[test]
    fn a_derived_image_that_cannot_be_made_falls_back_to_the_original() {
        let mut images = Images::new();
        let huge = ImageUse {
            cols: 297,
            rows: 1,
            look: look(whole(), Sizing::Stretch),
            ..images.image(8, 4)
        };
        let broken = ImageUse {
            cols: 1,
            rows: 1,
            look: look(right_half(), Sizing::Native),
            ..images.image(8, 4)
        };
        let mut uploader = images.uploader(Origin::Local, 1 << 30);
        uploader.set_cell_pixels(Some(CellPixels {
            width: 20,
            height: 20,
        }));
        uploader.frame(&[huge]);
        let ops = drain(&mut uploader);
        assert!(uploader.take_job().is_none());
        assert_eq!(uploaded(&ops, huge.key), *images.bytes(huge));

        uploader.frame(&[huge, broken]);
        assert!(drain(&mut uploader).is_empty());
        let ops = settle(&mut uploader);
        assert_eq!(
            summary(&ops),
            [('t', broken.key, 8, true), ('p', broken.key, 0, true)]
        );
        assert_eq!(uploaded(&ops, broken.key), *images.bytes(broken));
        assert_eq!(
            images.store.derived(
                broken.key,
                CellPixels {
                    width: 20,
                    height: 20
                }
            ),
            Some(Derived::Plain)
        );
    }

    #[test]
    fn derivations_run_one_at_a_time_and_go_with_their_image() {
        let mut images = Images::new();
        let first = images.pixels((1, 1), look(right_half(), Sizing::Native));
        let second = images.pixels((1, 1), look(right_half(), Sizing::Native));
        let mut uploader = images.uploader(Origin::Local, 1 << 30);
        uploader.frame(&[first, second]);
        assert!(drain(&mut uploader).is_empty());
        let job = uploader.take_job().unwrap();
        assert!(uploader.take_job().is_none());

        uploader.frame(&[]);
        uploader.finish(job.run());
        assert!(drain(&mut uploader).is_empty());
        assert!(uploader.take_job().is_none());
        assert!(!uploader.has_work());

        uploader.frame(&[second]);
        assert!(drain(&mut uploader).is_empty());
        assert!(uploader.take_job().is_some());
        uploader.abandon();
        let ops = drain(&mut uploader);
        assert_eq!(uploaded(&ops, second.key), *images.bytes(second));
        assert!(uploader.take_job().is_none());
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
