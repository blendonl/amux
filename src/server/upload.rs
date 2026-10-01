use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use tracing::debug;

use super::connection::Origin;
use super::graphics::animation::{Animation, Shape, Step};
use super::graphics::derive::{self, Plan};
use super::graphics::place::ASSUMED_CELL_PIXELS;
use super::graphics::store::{Derived, ImageData, ImageStore};
use super::render::{ImageUse, Viewer};
use crate::protocol::{AnimationControl, CellPixels, FrameSpec, ImageOp};

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
    syncs: BTreeSet<u32>,
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
    shape: Option<Shape>,
}

impl Held {
    fn moved(&self, store: &ImageStore) -> bool {
        self.shape
            .as_ref()
            .is_some_and(|shape| store.revision(self.shown.image) != Some(shape.revision()))
    }
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

struct Upload {
    shown: ImageUse,
    sends: VecDeque<Send>,
    offset: usize,
}

enum Send {
    Root(ImageData),
    Place,
    Frame(FrameSpec, ImageData),
    Animate(AnimationControl),
}

impl From<Step> for Send {
    fn from(step: Step) -> Self {
        match step {
            Step::Frame(spec, data) => Self::Frame(spec, data),
            Step::Animate(control) => Self::Animate(control),
        }
    }
}

impl Upload {
    fn key(&self) -> u32 {
        self.shown.key
    }

    fn end_early(mut self) -> Option<Self> {
        if self.offset == 0 {
            return None;
        }
        self.sends.truncate(1);
        self.offset = match self.sends.front()? {
            Send::Root(data) | Send::Frame(_, data) => data.bytes.len(),
            Send::Place | Send::Animate(_) => return None,
        };
        Some(self)
    }

    fn next(&mut self, chunk_len: usize) -> Option<ImageOp> {
        let key = self.shown.key;
        let (op, more) = match self.sends.front()? {
            Send::Root(data) => {
                let piece = Piece::of(data, self.offset, chunk_len);
                let op = ImageOp::Transmit {
                    key,
                    format: data.format,
                    width: data.width,
                    height: data.height,
                    compressed: data.compressed,
                    total: piece.total,
                    data: piece.data,
                    last: piece.more.is_none(),
                };
                (op, piece.more)
            }
            Send::Frame(spec, data) => {
                let piece = Piece::of(data, self.offset, chunk_len);
                let op = ImageOp::Frame {
                    key,
                    spec: *spec,
                    format: data.format,
                    width: data.width,
                    height: data.height,
                    compressed: data.compressed,
                    total: piece.total,
                    data: piece.data,
                    last: piece.more.is_none(),
                };
                (op, piece.more)
            }
            Send::Place => {
                let (cols, rows) = (self.shown.cols, self.shown.rows);
                (ImageOp::Place { key, cols, rows }, None)
            }
            Send::Animate(control) => {
                let control = *control;
                (ImageOp::Animate { key, control }, None)
            }
        };
        match more {
            Some(end) => self.offset = end,
            None => {
                self.sends.pop_front();
                self.offset = 0;
            }
        }
        Some(op)
    }
}

struct Piece {
    total: u32,
    data: Vec<u8>,
    more: Option<usize>,
}

impl Piece {
    fn of(data: &ImageData, offset: usize, chunk_len: usize) -> Self {
        let len = data.bytes.len();
        let end = len.min(offset + chunk_len);
        Self {
            total: u32::try_from(len).unwrap_or(u32::MAX),
            data: data.bytes[offset..end].to_vec(),
            more: (end < len).then_some(end),
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
            syncs: BTreeSet::new(),
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
        self.upload = self.upload.take().and_then(Upload::end_early);
        self.deletes.extend(self.held.keys());
        self.held.clear();
        self.used = 0;
        self.current.clear();
        self.refused.clear();
        self.queue.clear();
        self.syncs.clear();
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
        self.syncs.clear();
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
            let (fresh, moved) = match self.held.get_mut(&shown.key) {
                Some(held) => {
                    held.used = self.clock;
                    (!held.stale, held.moved(&self.store))
                }
                None => (false, false),
            };
            let waiting = self.queue.iter().any(|queued| queued.key == shown.key)
                || self.upload.as_ref().map(Upload::key) == Some(shown.key)
                || self.deriving.contains_key(&shown.key);
            if !fresh && !waiting {
                self.queue.push_back(*shown);
            } else if fresh && moved && !waiting {
                self.syncs.insert(shown.key);
            }
        }
    }

    pub fn has_work(&self) -> bool {
        !self.deletes.is_empty()
            || self.upload.is_some()
            || !self.queue.is_empty()
            || !self.syncs.is_empty()
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
        let upload = self.upload.as_mut()?;
        let op = upload.next(self.chunk_len);
        if upload.sends.is_empty() {
            let key = upload.key();
            self.upload = None;
            if self
                .held
                .get(&key)
                .is_some_and(|held| held.moved(&self.store))
            {
                self.syncs.insert(key);
            }
        }
        op
    }

    fn start(&mut self) {
        if self.sync() {
            return;
        }
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
            let (data, animation) = match plan.filter(Plan::fits) {
                None => self.plain(&shown, source),
                Some(plan) => match self
                    .ready
                    .remove(&shown.key)
                    .or_else(|| self.store.derived(shown.key, self.cell))
                {
                    Some(Derived::Image(derived)) => (derived, None),
                    Some(Derived::Plain) => self.plain(&shown, source),
                    None => {
                        self.request(shown, plan, source);
                        continue;
                    }
                },
            };
            let shape = animation.as_ref().map(Animation::shape);
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
                        shape,
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
                        shape,
                    },
                );
            } else {
                self.refused.insert(shown.key);
                continue;
            }
            let mut sends = VecDeque::from([Send::Root(data)]);
            if self.graphics {
                sends.push_back(Send::Place);
            }
            if let Some(animation) = animation {
                sends.extend(animation.steps().into_iter().map(Send::from));
            }
            self.upload = Some(Upload {
                shown,
                sends,
                offset: 0,
            });
            return;
        }
    }

    fn plain(&self, shown: &ImageUse, source: ImageData) -> (ImageData, Option<Animation>) {
        match self.store.animation(shown.image) {
            Some(animation) => (animation.root().clone(), Some(animation)),
            None => (source, None),
        }
    }

    fn sync(&mut self) -> bool {
        while let Some(key) = self.syncs.pop_first() {
            if !self.current.contains(&key) {
                continue;
            }
            let Some(held) = self.held.get_mut(&key) else {
                continue;
            };
            let Some(shape) = held.shape.as_ref().filter(|_| !held.stale) else {
                continue;
            };
            let Some(animation) = self.store.animation(held.shown.image) else {
                continue;
            };
            let Some(steps) = animation.steps_since(shape) else {
                held.stale = true;
                self.queue.push_back(held.shown);
                continue;
            };
            held.shape = Some(animation.shape());
            if steps.is_empty() {
                continue;
            }
            self.upload = Some(Upload {
                shown: held.shown,
                sends: steps.into_iter().map(Send::from).collect(),
                offset: 0,
            });
            return true;
        }
        false
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
    use crate::protocol::{AnimationState, ImageFormat};
    use crate::server::graphics::derive::{Look, Sizing};
    use crate::server::graphics::place::{CellOffset, SourceRect};
    use crate::server::graphics::store::{Buffer, ImageKey, PaneImages};
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

        fn animate(&self, shown: ImageUse, change: impl FnOnce(&mut Animation)) {
            let mut animation = self.store.animation(shown.image).unwrap();
            let read_at = animation.revision();
            change(&mut animation);
            self.pane.animate(shown.image, read_at, animation).unwrap();
        }
    }

    fn frame_data(len: usize) -> ImageData {
        ImageData {
            key: ImageKey(0),
            width: 1,
            height: 1,
            format: ImageFormat::Png,
            compressed: false,
            bytes: (0..len).map(|at| (at % 7) as u8).collect(),
            decoded_len: 4,
        }
    }

    fn add(len: usize) -> impl FnOnce(&mut Animation) {
        move |animation| {
            animation
                .add(FrameSpec::default(), frame_data(len))
                .unwrap();
        }
    }

    fn running() -> AnimationControl {
        AnimationControl {
            state: Some(AnimationState::Running),
            ..AnimationControl::default()
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
                ImageOp::Frame {
                    key, data, last, ..
                } => ('f', *key, data.len(), *last),
                ImageOp::Animate { key, .. } => ('a', *key, 0, true),
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
    fn an_animated_image_is_sent_with_its_frames_and_then_its_state() {
        let mut images = Images::new();
        let shown = images.image(8, 4);
        images.animate(shown, |animation| {
            let gapped = FrameSpec {
                gap: 70,
                ..FrameSpec::default()
            };
            animation.add(gapped, frame_data(5)).unwrap();
            let based = FrameSpec {
                base: 1,
                x: 1,
                ..FrameSpec::default()
            };
            animation.add(based, frame_data(3)).unwrap();
            animation.control(AnimationControl {
                loops: 1,
                ..running()
            });
        });
        let mut uploader = images.uploader(Origin::Local, 100);
        uploader.frame(&[shown]);
        let ops = drain(&mut uploader);
        let key = shown.key;
        assert_eq!(
            summary(&ops),
            [
                ('t', key, 8, true),
                ('p', key, 0, true),
                ('f', key, 5, true),
                ('f', key, 3, true),
                ('a', key, 0, true),
            ]
        );
        assert_eq!(
            ops[2],
            ImageOp::Frame {
                key,
                spec: FrameSpec {
                    gap: 70,
                    ..FrameSpec::default()
                },
                format: ImageFormat::Png,
                width: 1,
                height: 1,
                compressed: false,
                total: 5,
                data: vec![0, 1, 2, 3, 4],
                last: true,
            }
        );
        let ImageOp::Frame { spec, .. } = &ops[3] else {
            panic!("expected a frame, got {:?}", ops[3]);
        };
        assert_eq!(
            *spec,
            FrameSpec {
                base: 1,
                x: 1,
                gap: 40,
                ..FrameSpec::default()
            }
        );
        assert_eq!(
            ops[4],
            ImageOp::Animate {
                key,
                control: running()
            }
        );
        uploader.frame(&[shown]);
        assert!(!uploader.has_work());
        assert!(drain(&mut uploader).is_empty());
    }

    #[test]
    fn frames_go_in_the_same_chunks_as_images() {
        let mut images = Images::new();
        let shown = images.image(8, 4);
        images.animate(shown, add(150 * 1024));
        let mut uploader = images.uploader(Origin::Peer, 1 << 30);
        uploader.frame(&[shown]);
        let key = shown.key;
        assert_eq!(
            summary(&drain(&mut uploader)),
            [
                ('t', key, 8, true),
                ('p', key, 0, true),
                ('f', key, 64 * 1024, false),
                ('f', key, 64 * 1024, false),
                ('f', key, 22 * 1024, true),
            ]
        );
    }

    #[test]
    fn later_frames_and_controls_reach_a_client_that_holds_the_image() {
        let mut images = Images::new();
        let shown = images.image(8, 4);
        let elsewhere = images.image(8, 4);
        let mut uploader = images.uploader(Origin::Local, 100);
        uploader.frame(&[shown, elsewhere]);
        drain(&mut uploader);
        let key = shown.key;

        images.animate(shown, add(6));
        assert!(!uploader.has_work());
        uploader.frame(&[shown, elsewhere]);
        assert!(uploader.has_work());
        assert_eq!(summary(&drain(&mut uploader)), [('f', key, 6, true)]);

        images.animate(shown, |animation| animation.control(running()));
        images.animate(shown, add(2));
        images.animate(shown, |animation| {
            animation.control(AnimationControl {
                current: 3,
                ..AnimationControl::default()
            });
        });
        uploader.frame(&[shown, elsewhere]);
        let ops = drain(&mut uploader);
        assert_eq!(summary(&ops), [('f', key, 2, true), ('a', key, 0, true)]);
        assert_eq!(
            ops[1],
            ImageOp::Animate {
                key,
                control: AnimationControl {
                    current: 3,
                    ..running()
                }
            }
        );
        uploader.frame(&[shown, elsewhere]);
        assert!(drain(&mut uploader).is_empty());

        images.animate(shown, add(4));
        uploader.frame(&[elsewhere]);
        assert!(drain(&mut uploader).is_empty());
        uploader.frame(&[shown, elsewhere]);
        assert_eq!(summary(&drain(&mut uploader)), [('f', key, 4, true)]);
    }

    #[test]
    fn an_edited_frame_is_sent_whole_and_a_deleted_one_sends_the_image_again() {
        let mut images = Images::new();
        let shown = images.pixels((3, 2), None);
        let pixel = |colour: u8| Image {
            width: 1,
            height: 1,
            format: ImageFormat::Rgba32,
            compressed: false,
            bytes: vec![colour, 0, 0, 255],
            decoded_len: 4,
        };
        images.animate(shown, |animation| {
            let data = ImageData::new(shown.image, pixel(9));
            animation.add(FrameSpec::default(), data).unwrap();
        });
        let mut uploader = images.uploader(Origin::Local, 100);
        uploader.frame(&[shown]);
        let key = shown.key;
        assert_eq!(
            order(&drain(&mut uploader)),
            [('t', key), ('p', key), ('f', key)]
        );

        images.animate(shown, |animation| {
            let edit = FrameSpec {
                edit: 2,
                replace: true,
                ..FrameSpec::default()
            };
            let data = ImageData::new(shown.image, pixel(5));
            animation.add(edit, data).unwrap();
        });
        uploader.frame(&[shown]);
        let ops = drain(&mut uploader);
        let [ImageOp::Frame {
            spec,
            format,
            width,
            height,
            compressed,
            data,
            ..
        }] = ops.as_slice()
        else {
            panic!("expected one frame, got {ops:?}");
        };
        assert_eq!(
            *spec,
            FrameSpec {
                edit: 2,
                replace: true,
                ..FrameSpec::default()
            }
        );
        assert_eq!(
            (*format, *width, *height, *compressed),
            (ImageFormat::Rgba32, 4, 2, false)
        );
        assert_eq!(data[..8], [5, 0, 0, 255, 0, 0, 0, 0]);

        images.animate(shown, |animation| {
            animation.remove(2).unwrap();
        });
        uploader.frame(&[shown]);
        let ops = drain(&mut uploader);
        assert_eq!(order(&ops), [('t', key), ('p', key)]);
        assert_eq!(uploaded(&ops, key), *images.bytes(shown));
        uploader.frame(&[shown]);
        assert!(drain(&mut uploader).is_empty());
    }

    #[test]
    fn frames_that_arrive_during_the_upload_follow_it() {
        let mut images = Images::new();
        let shown = images.image(2 * MIB, 4);
        let mut uploader = images.uploader(Origin::Local, 100);
        uploader.frame(&[shown]);
        let key = shown.key;
        assert_eq!(
            summary(&[uploader.next().unwrap()]),
            [('t', key, MIB, false)]
        );
        images.animate(shown, add(5));
        uploader.frame(&[shown]);
        assert_eq!(
            summary(&drain(&mut uploader)),
            [
                ('t', key, MIB, true),
                ('p', key, 0, true),
                ('f', key, 5, true)
            ]
        );
        assert!(!uploader.has_work());
    }

    #[test]
    fn turning_graphics_off_ends_a_frame_transmission() {
        let mut images = Images::new();
        let shown = images.image(8, 4);
        images.animate(shown, add(2 * MIB));
        let mut uploader = images.uploader(Origin::Local, 100);
        uploader.frame(&[shown]);
        let key = shown.key;
        let ops: Vec<ImageOp> = std::iter::from_fn(|| uploader.next()).take(3).collect();
        assert_eq!(
            summary(&ops),
            [
                ('t', key, 8, true),
                ('p', key, 0, true),
                ('f', key, MIB, false)
            ]
        );
        uploader.set_graphics(false);
        assert_eq!(
            summary(&drain(&mut uploader)),
            [('d', key, 0, true), ('f', key, 0, true)]
        );
    }

    #[test]
    fn a_derived_image_is_not_animated() {
        let mut images = Images::new();
        let shown = images.pixels((1, 1), look(right_half(), Sizing::Native));
        images.animate(shown, |animation| {
            add(3)(animation);
            animation.control(running());
        });
        let mut uploader = images.uploader(Origin::Local, 1 << 30);
        uploader.set_cell_pixels(Some(SMALL));
        uploader.frame(&[shown]);
        let ops = settle(&mut uploader);
        assert_eq!(order(&ops), [('t', shown.key), ('p', shown.key)]);
        images.animate(shown, add(4));
        uploader.frame(&[shown]);
        assert!(settle(&mut uploader).is_empty());
        assert!(!uploader.has_work());
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
