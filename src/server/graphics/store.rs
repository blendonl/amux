use std::collections::{HashMap, HashSet};
use std::ops::RangeInclusive;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use super::respond::{Code, Failure};
use super::transmit::Image;
use crate::protocol::{CellPixels, ImageFormat};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Buffer {
    Main,
    Alt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Name {
    Id(u32),
    Number(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ImageKey(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Owner(u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageData {
    pub key: ImageKey,
    pub width: u32,
    pub height: u32,
    pub format: ImageFormat,
    pub compressed: bool,
    pub bytes: Arc<[u8]>,
    pub decoded_len: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Derived {
    Image(ImageData),
    Plain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stored {
    pub key: ImageKey,
    pub id: u32,
    pub width: u32,
    pub height: u32,
}

pub struct ImageStore {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    quota: usize,
    used: usize,
    last_key: u32,
    last_owner: u64,
    last_display: u32,
    clock: u64,
    owners: HashSet<Owner>,
    images: HashMap<ImageKey, Entry>,
    names: HashMap<(Owner, Buffer, Name), ImageKey>,
    displays: HashMap<u32, Display>,
}

struct Display {
    owner: Owner,
    derived: Option<Cached>,
}

struct Cached {
    cell: CellPixels,
    derived: Derived,
    used: u64,
}

impl Cached {
    fn len(&self) -> usize {
        match &self.derived {
            Derived::Image(data) => data.bytes.len(),
            Derived::Plain => 0,
        }
    }
}

struct Entry {
    data: ImageData,
    owner: Owner,
    buffer: Buffer,
    id: u32,
    used: u64,
    placements: u32,
}

impl ImageStore {
    pub fn new(quota: u64) -> Self {
        Self {
            state: Mutex::new(State {
                quota: bytes(quota),
                ..State::default()
            }),
        }
    }

    pub fn set_quota(&self, quota: u64) {
        let mut state = self.lock();
        state.quota = bytes(quota);
        state.evict(None);
    }

    pub fn open_pane(self: &Arc<Self>) -> PaneImages {
        let mut state = self.lock();
        state.last_owner += 1;
        let owner = Owner(state.last_owner);
        state.owners.insert(owner);
        PaneImages {
            store: Arc::clone(self),
            owner,
        }
    }

    pub fn get(&self, key: ImageKey) -> Option<ImageData> {
        let mut state = self.lock();
        let clock = state.tick();
        let entry = state.images.get_mut(&key)?;
        entry.used = clock;
        Some(entry.data.clone())
    }

    pub fn is_displayed(&self, display: u32) -> bool {
        self.lock().displays.contains_key(&display)
    }

    #[cfg(test)]
    pub fn skip_display_keys(&self, last: u32) {
        self.lock().last_display = last;
    }

    pub fn derived(&self, display: u32, cell: CellPixels) -> Option<Derived> {
        let mut state = self.lock();
        let clock = state.tick();
        let cached = state
            .displays
            .get_mut(&display)?
            .derived
            .as_mut()
            .filter(|cached| cached.cell == cell)?;
        cached.used = clock;
        Some(cached.derived.clone())
    }

    pub fn keep_derived(&self, display: u32, cell: CellPixels, derived: Derived) -> Derived {
        let mut state = self.lock();
        let Some(shown) = state.displays.get_mut(&display) else {
            return Derived::Plain;
        };
        let freed = shown.derived.take().map_or(0, |old| old.len());
        state.used -= freed;
        let derived = match derived {
            Derived::Image(data) if state.make_derived_room(data.bytes.len()) => {
                Derived::Image(data)
            }
            Derived::Image(_) | Derived::Plain => Derived::Plain,
        };
        let cached = Cached {
            cell,
            derived: derived.clone(),
            used: state.tick(),
        };
        state.used += cached.len();
        if let Some(shown) = state.displays.get_mut(&display) {
            shown.derived = Some(cached);
        }
        derived
    }

    pub fn add_placement(&self, key: ImageKey) -> bool {
        let mut state = self.lock();
        let clock = state.tick();
        let Some(entry) = state.images.get_mut(&key) else {
            return false;
        };
        entry.placements += 1;
        entry.used = clock;
        true
    }

    pub fn remove_placement(&self, key: ImageKey, free: bool) {
        let mut state = self.lock();
        let Some(entry) = state.images.get_mut(&key) else {
            return;
        };
        entry.placements = entry.placements.saturating_sub(1);
        if entry.placements == 0 && (free || entry.id == 0) {
            state.remove(key);
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl State {
    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    fn insert(
        &mut self,
        owner: Owner,
        buffer: Buffer,
        name: (u32, u32),
        image: Image,
    ) -> Result<Stored, Failure> {
        if !self.owners.contains(&owner) {
            return Err(Failure::new(Code::Einval, "the pane has closed"));
        }
        let len = image.bytes.len();
        if len > self.quota {
            return Err(Failure::new(
                Code::Efbig,
                "the image is larger than images.memory_mb",
            ));
        }
        let key = self
            .last_key
            .checked_add(1)
            .map(ImageKey)
            .ok_or_else(|| Failure::new(Code::Efbig, "out of image keys"))?;
        self.last_key = key.0;
        let (id, number) = name;
        let id = match (id, number) {
            (0, 0) => 0,
            (0, _) => self.free_id(owner, buffer),
            (id, _) => id,
        };
        if id != 0 {
            if let Some(replaced) = self.names.insert((owner, buffer, Name::Id(id)), key) {
                self.remove(replaced);
            }
        }
        if number != 0 {
            self.names
                .insert((owner, buffer, Name::Number(number)), key);
        }
        let used = self.tick();
        let (width, height) = (image.width, image.height);
        let data = ImageData {
            key,
            width,
            height,
            format: image.format,
            compressed: image.compressed,
            bytes: image.bytes.into(),
            decoded_len: image.decoded_len,
        };
        self.images.insert(
            key,
            Entry {
                data,
                owner,
                buffer,
                id,
                used,
                placements: 0,
            },
        );
        self.used += len;
        self.evict(Some(key));
        Ok(Stored {
            key,
            id,
            width,
            height,
        })
    }

    fn mint_display(&mut self, owner: Owner) -> Result<u32, Failure> {
        let display = self
            .last_display
            .checked_add(1)
            .ok_or_else(|| Failure::new(Code::Efbig, "out of display keys"))?;
        self.last_display = display;
        self.displays.insert(
            display,
            Display {
                owner,
                derived: None,
            },
        );
        Ok(display)
    }

    fn release_display(&mut self, display: u32) {
        if let Some(cached) = self
            .displays
            .remove(&display)
            .and_then(|shown| shown.derived)
        {
            self.used -= cached.len();
        }
    }

    fn free_id(&self, owner: Owner, buffer: Buffer) -> u32 {
        (1..=u32::MAX)
            .find(|id| !self.names.contains_key(&(owner, buffer, Name::Id(*id))))
            .unwrap_or(0)
    }

    fn lookup(&mut self, owner: Owner, buffer: Buffer, name: Name) -> Option<ImageKey> {
        let key = *self.names.get(&(owner, buffer, name))?;
        let clock = self.tick();
        if let Some(entry) = self.images.get_mut(&key) {
            entry.used = clock;
        }
        Some(key)
    }

    fn find(&mut self, owner: Owner, buffer: Buffer, name: Name) -> Option<Stored> {
        let key = self.lookup(owner, buffer, name)?;
        let entry = self.images.get(&key)?;
        Some(Stored {
            key,
            id: entry.id,
            width: entry.data.width,
            height: entry.data.height,
        })
    }

    fn remove(&mut self, key: ImageKey) -> bool {
        let Some(entry) = self.images.remove(&key) else {
            return false;
        };
        self.used -= entry.data.bytes.len();
        self.names.retain(|_, named| *named != key);
        true
    }

    fn evict(&mut self, keep: Option<ImageKey>) {
        self.evict_derived(0);
        while self.used > self.quota {
            let victim = self
                .images
                .iter()
                .filter(|(key, _)| Some(**key) != keep)
                .min_by_key(|(_, entry)| (entry.placements > 0, entry.used))
                .map(|(key, _)| *key);
            match victim {
                Some(victim) => self.remove(victim),
                None => break,
            };
        }
    }

    fn make_derived_room(&mut self, room: usize) -> bool {
        let derived: usize = self
            .displays
            .values()
            .filter_map(|shown| shown.derived.as_ref())
            .map(Cached::len)
            .sum();
        (self.used - derived).saturating_add(room) <= self.quota && self.evict_derived(room)
    }

    fn evict_derived(&mut self, room: usize) -> bool {
        while self.used.saturating_add(room) > self.quota {
            let victim = self
                .displays
                .values_mut()
                .filter(|shown| {
                    shown
                        .derived
                        .as_ref()
                        .is_some_and(|cached| cached.len() > 0)
                })
                .min_by_key(|shown| shown.derived.as_ref().map(|cached| cached.used));
            let Some(cached) = victim.and_then(|shown| shown.derived.take()) else {
                return false;
            };
            self.used -= cached.len();
        }
        true
    }

    fn close(&mut self, owner: Owner) {
        self.owners.remove(&owner);
        let mut freed = 0;
        self.images.retain(|_, entry| {
            let kept = entry.owner != owner;
            if !kept {
                freed += entry.data.bytes.len();
            }
            kept
        });
        self.names.retain(|(named, _, _), _| *named != owner);
        self.displays.retain(|_, shown| {
            let kept = shown.owner != owner;
            if !kept {
                freed += shown.derived.as_ref().map_or(0, Cached::len);
            }
            kept
        });
        self.used -= freed;
    }
}

#[derive(Clone)]
pub struct PaneImages {
    store: Arc<ImageStore>,
    owner: Owner,
}

impl PaneImages {
    pub fn insert(
        &self,
        buffer: Buffer,
        id: u32,
        number: u32,
        image: Image,
    ) -> Result<Stored, Failure> {
        self.store
            .lock()
            .insert(self.owner, buffer, (id, number), image)
    }

    pub fn lookup(&self, buffer: Buffer, name: Name) -> Option<ImageKey> {
        self.store.lock().lookup(self.owner, buffer, name)
    }

    pub fn find(&self, buffer: Buffer, name: Name) -> Option<Stored> {
        self.store.lock().find(self.owner, buffer, name)
    }

    pub fn add_placement(&self, key: ImageKey) -> bool {
        self.store.add_placement(key)
    }

    pub fn remove_placement(&self, key: ImageKey, free: bool) {
        self.store.remove_placement(key, free);
    }

    pub fn mint_display(&self) -> Result<u32, Failure> {
        self.store.lock().mint_display(self.owner)
    }

    pub fn release_display(&self, display: u32) {
        self.store.lock().release_display(display);
    }

    pub fn is_stored(&self, key: ImageKey) -> bool {
        self.store.get(key).is_some()
    }

    pub fn free(&self, buffer: Buffer, name: Name) -> bool {
        let mut state = self.store.lock();
        match state.names.get(&(self.owner, buffer, name)) {
            Some(&key) => state.remove(key),
            None => false,
        }
    }

    pub fn free_ids(&self, buffer: Buffer, ids: RangeInclusive<u32>) {
        self.free_where(buffer, |entry| entry.id != 0 && ids.contains(&entry.id));
    }

    pub fn free_unplaced(&self, buffer: Buffer) {
        self.free_where(buffer, |entry| entry.placements == 0);
    }

    pub fn close(&self) {
        self.store.lock().close(self.owner);
    }

    fn free_where(&self, buffer: Buffer, doomed: impl Fn(&Entry) -> bool) {
        let mut state = self.store.lock();
        let keys: Vec<ImageKey> = state
            .images
            .iter()
            .filter(|(_, entry)| entry.owner == self.owner && entry.buffer == buffer)
            .filter(|(_, entry)| doomed(entry))
            .map(|(key, _)| *key)
            .collect();
        for key in keys {
            state.remove(key);
        }
    }
}

fn bytes(quota: u64) -> usize {
    usize::try_from(quota).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(len: usize) -> Image {
        Image {
            width: 1,
            height: 1,
            format: ImageFormat::Png,
            compressed: false,
            bytes: vec![7; len],
            decoded_len: 4,
        }
    }

    fn store(quota: u64) -> Arc<ImageStore> {
        Arc::new(ImageStore::new(quota))
    }

    fn used(store: &ImageStore) -> usize {
        store.lock().used
    }

    fn keys(store: &ImageStore) -> Vec<u32> {
        let mut keys: Vec<u32> = store.lock().images.keys().map(|key| key.0).collect();
        keys.sort_unstable();
        keys
    }

    #[test]
    fn keys_count_up_from_one_and_are_never_reused() {
        let store = store(1 << 20);
        let pane = store.open_pane();
        let first = pane.insert(Buffer::Main, 1, 0, image(10)).unwrap();
        let second = pane.insert(Buffer::Main, 2, 0, image(10)).unwrap();
        assert_eq!((first.key, second.key), (ImageKey(1), ImageKey(2)));
        assert!(pane.free(Buffer::Main, Name::Id(2)));
        let third = pane.insert(Buffer::Main, 2, 0, image(10)).unwrap();
        assert_eq!(third.key, ImageKey(3));
        assert_eq!(third.id, 2);
    }

    #[test]
    fn sending_an_id_again_replaces_the_image_under_a_new_key() {
        let store = store(1 << 20);
        let pane = store.open_pane();
        let old = pane.insert(Buffer::Main, 5, 0, image(10)).unwrap().key;
        let new = pane.insert(Buffer::Main, 5, 0, image(30)).unwrap().key;
        assert_ne!(old, new);
        assert_eq!(store.get(old), None);
        assert_eq!(pane.lookup(Buffer::Main, Name::Id(5)), Some(new));
        assert_eq!(store.get(new).unwrap().bytes.len(), 30);
        assert_eq!(used(&store), 30);
    }

    #[test]
    fn stored_data_keeps_what_the_transmission_decoded() {
        let store = store(1 << 20);
        let pane = store.open_pane();
        let sent = Image {
            width: 3,
            height: 2,
            format: ImageFormat::Rgb24,
            compressed: true,
            bytes: vec![1, 2, 3],
            decoded_len: 18,
        };
        let key = pane.insert(Buffer::Main, 1, 0, sent).unwrap().key;
        assert_eq!(
            store.get(key),
            Some(ImageData {
                key,
                width: 3,
                height: 2,
                format: ImageFormat::Rgb24,
                compressed: true,
                bytes: vec![1, 2, 3].into(),
                decoded_len: 18,
            })
        );
    }

    #[test]
    fn a_number_alone_gets_the_lowest_free_id() {
        let store = store(1 << 20);
        let pane = store.open_pane();
        pane.insert(Buffer::Main, 1, 0, image(1)).unwrap();
        let first = pane.insert(Buffer::Main, 0, 7, image(1)).unwrap();
        let second = pane.insert(Buffer::Main, 0, 7, image(1)).unwrap();
        assert_eq!((first.id, second.id), (2, 3));
        assert_eq!(pane.lookup(Buffer::Main, Name::Number(7)), Some(second.key));
        assert_eq!(pane.lookup(Buffer::Main, Name::Id(2)), Some(first.key));
        assert!(pane.free(Buffer::Main, Name::Number(7)));
        assert_eq!(pane.lookup(Buffer::Main, Name::Id(3)), None);
        assert_eq!(pane.lookup(Buffer::Main, Name::Id(2)), Some(first.key));

        let anonymous = pane.insert(Buffer::Main, 0, 0, image(1)).unwrap();
        assert_eq!(anonymous.id, 0);
        assert!(store.get(anonymous.key).is_some());
    }

    #[test]
    fn panes_and_screen_buffers_have_separate_names() {
        let store = store(1 << 20);
        let left = store.open_pane();
        let right = store.open_pane();
        let main = left.insert(Buffer::Main, 1, 0, image(1)).unwrap().key;
        let alt = left.insert(Buffer::Alt, 1, 0, image(1)).unwrap().key;
        let other = right.insert(Buffer::Main, 1, 0, image(1)).unwrap().key;
        assert_eq!(left.lookup(Buffer::Main, Name::Id(1)), Some(main));
        assert_eq!(left.lookup(Buffer::Alt, Name::Id(1)), Some(alt));
        assert_eq!(right.lookup(Buffer::Main, Name::Id(1)), Some(other));
        assert_eq!(right.lookup(Buffer::Alt, Name::Id(1)), None);

        assert!(left.free(Buffer::Alt, Name::Id(1)));
        assert!(!left.free(Buffer::Alt, Name::Id(1)));
        assert_eq!(keys(&store), [main.0, other.0]);
    }

    #[test]
    fn closing_a_pane_drops_its_names_and_images() {
        let store = store(1 << 20);
        let closing = store.open_pane();
        let staying = store.open_pane();
        let gone = closing.insert(Buffer::Main, 1, 0, image(10)).unwrap().key;
        closing.insert(Buffer::Alt, 0, 3, image(10)).unwrap();
        closing.insert(Buffer::Main, 0, 0, image(10)).unwrap();
        let kept = staying.insert(Buffer::Main, 1, 0, image(5)).unwrap().key;

        closing.close();
        assert_eq!(store.get(gone), None);
        assert_eq!(keys(&store), [kept.0]);
        assert_eq!(used(&store), 5);
        assert_eq!(closing.lookup(Buffer::Main, Name::Id(1)), None);
        assert!(closing.insert(Buffer::Main, 2, 0, image(1)).is_err());
        assert_eq!(staying.lookup(Buffer::Main, Name::Id(1)), Some(kept));
    }

    #[test]
    fn a_full_store_evicts_unplaced_images_least_recently_used_first() {
        let store = store(300);
        let pane = store.open_pane();
        let insert = |id| pane.insert(Buffer::Main, id, 0, image(100)).unwrap().key;
        let first = insert(1);
        let second = insert(2);
        let third = insert(3);
        assert!(store.get(first).is_some());
        assert!(store.add_placement(second));

        let fourth = insert(4);
        assert_eq!(store.get(third), None);
        assert_eq!(keys(&store), [first.0, second.0, fourth.0]);
        let fifth = insert(5);
        assert_eq!(keys(&store), [second.0, fourth.0, fifth.0]);
        let sixth = insert(6);
        assert_eq!(keys(&store), [second.0, fifth.0, sixth.0]);
        assert_eq!(pane.lookup(Buffer::Main, Name::Id(4)), None);

        store.remove_placement(second, false);
        assert!(store.get(fifth).is_some());
        insert(7);
        assert_eq!(keys(&store), [fifth.0, sixth.0, 7]);
        assert_eq!(used(&store), 300);
    }

    #[test]
    fn placed_images_go_last_but_still_go_when_nothing_else_is_left() {
        let store = store(200);
        let pane = store.open_pane();
        let placed = pane.insert(Buffer::Main, 1, 0, image(100)).unwrap().key;
        store.add_placement(placed);
        pane.insert(Buffer::Main, 2, 0, image(100)).unwrap();
        let big = pane.insert(Buffer::Main, 3, 0, image(200)).unwrap().key;
        assert_eq!(keys(&store), [big.0]);
    }

    #[test]
    fn find_reports_the_id_and_size_of_a_named_image() {
        let store = store(1 << 20);
        let pane = store.open_pane();
        let numbered = pane.insert(Buffer::Main, 0, 7, image(1)).unwrap();
        assert_eq!(
            numbered,
            Stored {
                key: numbered.key,
                id: 1,
                width: 1,
                height: 1
            }
        );
        assert_eq!(pane.find(Buffer::Main, Name::Number(7)), Some(numbered));
        assert_eq!(pane.find(Buffer::Main, Name::Id(1)), Some(numbered));
        assert_eq!(pane.find(Buffer::Alt, Name::Id(1)), None);
    }

    #[test]
    fn the_last_placement_frees_an_anonymous_image_or_one_asked_to_be_freed() {
        let store = store(1 << 20);
        let pane = store.open_pane();
        let named = pane.insert(Buffer::Main, 1, 0, image(1)).unwrap().key;
        let anonymous = pane.insert(Buffer::Main, 0, 0, image(1)).unwrap().key;
        for key in [named, anonymous] {
            assert!(pane.add_placement(key));
            assert!(pane.add_placement(key));
            pane.remove_placement(key, false);
        }
        assert!(store.get(anonymous).is_some());
        pane.remove_placement(named, false);
        pane.remove_placement(anonymous, false);
        assert!(store.get(named).is_some());
        assert!(store.get(anonymous).is_none());

        assert!(pane.add_placement(named));
        pane.remove_placement(named, true);
        assert!(store.get(named).is_none());
        assert!(!pane.add_placement(named));
    }

    #[test]
    fn freeing_unplaced_images_or_an_id_range_stays_in_its_pane_and_screen() {
        let store = store(1 << 20);
        let pane = store.open_pane();
        let other = store.open_pane();
        let placed = pane.insert(Buffer::Main, 1, 0, image(1)).unwrap().key;
        let loose = pane.insert(Buffer::Main, 2, 0, image(1)).unwrap().key;
        let alt = pane.insert(Buffer::Alt, 3, 0, image(1)).unwrap().key;
        let elsewhere = other.insert(Buffer::Main, 4, 0, image(1)).unwrap().key;
        assert!(pane.add_placement(placed));

        pane.free_unplaced(Buffer::Main);
        assert_eq!(store.get(loose), None);
        assert_eq!(keys(&store), [placed.0, alt.0, elsewhere.0]);
        pane.free_ids(Buffer::Main, 2..=4);
        assert_eq!(keys(&store), [placed.0, alt.0, elsewhere.0]);
        pane.free_ids(Buffer::Main, 1..=1);
        assert_eq!(keys(&store), [alt.0, elsewhere.0]);
        pane.free_ids(Buffer::Alt, 0..=u32::MAX);
        assert_eq!(keys(&store), [elsewhere.0]);
    }

    #[test]
    fn display_keys_count_up_across_panes_and_die_with_their_pane() {
        let store = store(1 << 20);
        let left = store.open_pane();
        let right = store.open_pane();
        let first = left.mint_display().unwrap();
        let second = right.mint_display().unwrap();
        let third = left.mint_display().unwrap();
        assert_eq!((first, second, third), (1, 2, 3));
        assert!([first, second, third]
            .iter()
            .all(|display| store.is_displayed(*display)));

        left.release_display(first);
        assert!(!store.is_displayed(first));
        assert_eq!(right.mint_display().unwrap(), 4);
        left.close();
        assert!(!store.is_displayed(third));
        assert!(store.is_displayed(second));
    }

    const CELL: CellPixels = CellPixels {
        width: 10,
        height: 20,
    };
    const OTHER_CELL: CellPixels = CellPixels {
        width: 8,
        height: 16,
    };

    fn derived_image(len: usize) -> Derived {
        Derived::Image(ImageData {
            key: ImageKey(0),
            width: 1,
            height: 1,
            format: ImageFormat::Rgba32,
            compressed: true,
            bytes: vec![1; len].into(),
            decoded_len: 4,
        })
    }

    #[test]
    fn a_derived_image_is_kept_per_display_and_cell_size() {
        let store = store(1 << 20);
        let pane = store.open_pane();
        pane.insert(Buffer::Main, 1, 0, image(5)).unwrap();
        let display = pane.mint_display().unwrap();
        assert_eq!(store.derived(display, CELL), None);
        assert_eq!(
            store.keep_derived(display, CELL, derived_image(10)),
            derived_image(10)
        );
        assert_eq!(store.derived(display, CELL), Some(derived_image(10)));
        assert_eq!(store.derived(display, OTHER_CELL), None);
        assert_eq!(used(&store), 15);

        store.keep_derived(display, OTHER_CELL, derived_image(20));
        assert_eq!(store.derived(display, CELL), None);
        assert_eq!(store.derived(display, OTHER_CELL), Some(derived_image(20)));
        assert_eq!(used(&store), 25);
        assert_eq!(
            store.keep_derived(display, CELL, Derived::Plain),
            Derived::Plain
        );
        assert_eq!(store.derived(display, CELL), Some(Derived::Plain));
        assert_eq!(used(&store), 5);

        store.keep_derived(display, CELL, derived_image(10));
        pane.release_display(display);
        assert_eq!(store.derived(display, CELL), None);
        assert_eq!(used(&store), 5);
        assert_eq!(
            store.keep_derived(display, CELL, derived_image(10)),
            Derived::Plain
        );
        assert_eq!(used(&store), 5);
    }

    #[test]
    fn derived_images_count_against_the_quota_but_never_push_out_an_image() {
        let store = store(100);
        let pane = store.open_pane();
        let [first, second, third] = [(); 3].map(|()| pane.mint_display().unwrap());
        let placed = pane.insert(Buffer::Main, 1, 0, image(40)).unwrap().key;
        assert!(store.add_placement(placed));
        store.keep_derived(first, CELL, derived_image(30));
        store.keep_derived(second, CELL, derived_image(30));
        assert_eq!(used(&store), 100);
        store.derived(first, CELL);
        store.keep_derived(third, CELL, derived_image(30));
        assert_eq!(store.derived(second, CELL), None);
        assert_eq!(store.derived(first, CELL), Some(derived_image(30)));
        assert_eq!(used(&store), 100);

        let loose = pane.insert(Buffer::Main, 2, 0, image(20)).unwrap().key;
        assert_eq!(store.derived(third, CELL), None);
        assert_eq!(keys(&store), [placed.0, loose.0]);
        assert_eq!(used(&store), 90);

        assert_eq!(
            store.keep_derived(second, CELL, derived_image(80)),
            Derived::Plain
        );
        assert_eq!(store.derived(first, CELL), Some(derived_image(30)));
        assert_eq!(used(&store), 90);

        store.set_quota(60);
        assert_eq!(store.derived(first, CELL), None);
        assert_eq!(keys(&store), [placed.0, loose.0]);
        assert_eq!(used(&store), 60);
    }

    #[test]
    fn closing_a_pane_frees_its_derived_images() {
        let store = store(1 << 20);
        let closing = store.open_pane();
        let staying = store.open_pane();
        let gone = closing.mint_display().unwrap();
        let kept = staying.mint_display().unwrap();
        closing.insert(Buffer::Main, 1, 0, image(3)).unwrap();
        store.keep_derived(gone, CELL, derived_image(10));
        store.keep_derived(kept, CELL, derived_image(7));
        closing.close();
        assert_eq!(used(&store), 7);
        assert_eq!(store.derived(gone, CELL), None);
        assert_eq!(store.derived(kept, CELL), Some(derived_image(7)));
    }

    #[test]
    fn an_image_over_the_quota_is_refused_and_a_lower_quota_evicts() {
        let store = store(100);
        let pane = store.open_pane();
        let failure = pane.insert(Buffer::Main, 1, 0, image(101)).unwrap_err();
        assert_eq!(failure.code, Code::Efbig);
        let older = pane.insert(Buffer::Main, 1, 0, image(50)).unwrap().key;
        let newer = pane.insert(Buffer::Main, 2, 0, image(50)).unwrap().key;
        store.set_quota(60);
        assert_eq!(keys(&store), [newer.0]);
        assert_eq!(store.get(older), None);
        assert!(!store.add_placement(older));
    }
}
