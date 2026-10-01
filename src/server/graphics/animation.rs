use std::iter;
use std::mem;

use super::command::Composition;
use super::derive;
use super::respond::{Code, Failure};
use super::store::ImageData;
use crate::protocol::{AnimationControl, AnimationState, FrameSpec, ImageFormat};

const DEFAULT_GAP: u32 = 40;
const MAX_DEPTH: usize = 32;
const LONG_CHAIN: usize = 5;
const MAX_CANVAS_LEN: u64 = 256 * 1024 * 1024;
const RGB: usize = 3;
const RGBA: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Animation {
    revision: u64,
    width: u32,
    height: u32,
    last_id: u32,
    root: Frame,
    frames: Vec<Frame>,
    state: AnimationState,
    max_loops: u32,
    current: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Frame {
    id: u32,
    stamp: u64,
    gap: u32,
    content: Content,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Content {
    Whole(ImageData),
    Sent(Sent),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Sent {
    base: u32,
    x: u32,
    y: u32,
    background: u32,
    replace: bool,
    data: ImageData,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shape {
    revision: u64,
    frames: Vec<FrameShape>,
    state: AnimationState,
    max_loops: u32,
    current: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrameShape {
    id: u32,
    stamp: u64,
    gap: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Frame(FrameSpec, ImageData),
    Animate(AnimationControl),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Pixels {
    opaque: bool,
    data: Vec<u8>,
}

impl Frame {
    fn data(&self) -> &ImageData {
        match &self.content {
            Content::Whole(data) | Content::Sent(Sent { data, .. }) => data,
        }
    }

    fn base(&self) -> u32 {
        match &self.content {
            Content::Whole(_) => 0,
            Content::Sent(sent) => sent.base,
        }
    }

    fn shape(&self) -> FrameShape {
        FrameShape {
            id: self.id,
            stamp: self.stamp,
            gap: self.gap,
        }
    }
}

impl Shape {
    pub fn revision(&self) -> u64 {
        self.revision
    }
}

impl Animation {
    pub fn new(root: ImageData) -> Self {
        Self {
            revision: 0,
            width: root.width,
            height: root.height,
            last_id: 1,
            root: Frame {
                id: 1,
                stamp: 0,
                gap: 0,
                content: Content::Whole(root),
            },
            frames: Vec::new(),
            state: AnimationState::Stopped,
            max_loops: 0,
            current: 0,
        }
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn root(&self) -> &ImageData {
        self.root.data()
    }

    pub fn stored_len(&self) -> usize {
        let root = match self.root.stamp {
            0 => 0,
            _ => self.root.data().bytes.len(),
        };
        let frames: usize = self
            .frames
            .iter()
            .map(|frame| frame.data().bytes.len() + mem::size_of::<Frame>())
            .sum();
        root + frames
    }

    pub fn add(&mut self, spec: FrameSpec, data: ImageData) -> Result<u32, Failure> {
        if data.width > self.width {
            return Err(Failure::new(
                Code::Einval,
                format!(
                    "frame width {} larger than image width: {}",
                    data.width, self.width
                ),
            ));
        }
        if data.height > self.height {
            return Err(Failure::new(
                Code::Einval,
                format!(
                    "frame height {} larger than image height: {}",
                    data.height, self.height
                ),
            ));
        }
        let created = self.frames.len() + 2;
        let number = match usize::try_from(spec.edit).unwrap_or(usize::MAX) {
            0 => created,
            edit => edit.min(created),
        };
        let gap = gap_of(spec.gap);
        let stamp = self.revision + 1;
        if number == created {
            let content = self.created(spec, data)?;
            self.last_id += 1;
            self.frames.push(Frame {
                id: self.last_id,
                stamp,
                gap: gap.unwrap_or(DEFAULT_GAP),
                content,
            });
        } else {
            let frame = self.numbered(number).ok_or_else(|| no_frame(number))?;
            let mut canvas = self.coalesce(frame, 0)?;
            canvas.paste(
                self.size(),
                &unpack(&data)?,
                (data.width, data.height),
                (spec.x, spec.y),
                spec.replace,
            );
            let whole = self.whole(canvas);
            let frame = self.numbered_mut(number).ok_or_else(|| no_frame(number))?;
            if let Some(gap) = gap {
                frame.gap = gap;
            }
            frame.content = Content::Whole(whole);
            frame.stamp = stamp;
        }
        self.revision = stamp;
        Ok(u32::try_from(number).unwrap_or(u32::MAX))
    }

    pub fn control(&mut self, control: AnimationControl) {
        let mut changed = false;
        if control.gap != 0 {
            if let Some(frame) = self.numbered_mut(index_of(control.frame)) {
                let gap = u32::try_from(control.gap).unwrap_or(0);
                changed |= frame.gap != gap;
                frame.gap = gap;
            }
        }
        if control.current != 0 {
            let current = index_of(control.current) - 1;
            if current != self.current && current <= self.frames.len() {
                self.current = current;
                changed = true;
            }
        }
        if let Some(state) = control.state {
            changed |= self.state != state;
            self.state = state;
        }
        if control.loops != 0 {
            changed |= self.max_loops != control.loops - 1;
            self.max_loops = control.loops - 1;
        }
        if changed {
            self.revision += 1;
        }
    }

    pub fn compose(&mut self, composition: Composition) -> Result<(), Failure> {
        let missing = |which: &str, number: u32| {
            Failure::new(
                Code::Enoent,
                format!("no {which} frame number {number} exists"),
            )
        };
        let source = self
            .numbered(index_of(composition.source))
            .ok_or_else(|| missing("source", composition.source))?;
        let dest = self
            .numbered(index_of(composition.dest))
            .ok_or_else(|| missing("destination", composition.dest))?;
        let (image_width, image_height) = (u64::from(self.width), u64::from(self.height));
        let width = match composition.width {
            0 => image_width,
            width => u64::from(width),
        };
        let height = match composition.height {
            0 => image_height,
            height => u64::from(height),
        };
        let to = (u64::from(composition.x), u64::from(composition.y));
        let from = (
            u64::from(composition.source_x),
            u64::from(composition.source_y),
        );
        if to.0 + width > image_width || to.1 + height > image_height {
            return Err(Failure::new(
                Code::Einval,
                "the destination rectangle is out of bounds",
            ));
        }
        if from.0 + width > image_width || from.1 + height > image_height {
            return Err(Failure::new(
                Code::Einval,
                "the source rectangle is out of bounds",
            ));
        }
        let overlaps = |a: u64, b: u64, len: u64| a.max(b) < a.min(b) + len;
        if composition.source == composition.dest
            && overlaps(from.0, to.0, width)
            && overlaps(from.1, to.1, height)
        {
            return Err(Failure::new(
                Code::Einval,
                "the source and destination rectangles overlap in the same frame",
            ));
        }
        let over = self.coalesce(source, 0)?;
        let mut under = self.coalesce(dest, 0)?;
        let narrow = |value: u64| u32::try_from(value).unwrap_or(u32::MAX);
        under.copy_rect(
            self.width,
            &over,
            (narrow(width), narrow(height)),
            (narrow(from.0), narrow(from.1)),
            (narrow(to.0), narrow(to.1)),
            composition.replace,
        );
        let whole = self.whole(under);
        let stamp = self.revision + 1;
        let dest = index_of(composition.dest);
        let dest = self.numbered_mut(dest).ok_or_else(|| no_frame(dest))?;
        dest.content = Content::Whole(whole);
        dest.stamp = stamp;
        self.revision = stamp;
        Ok(())
    }

    pub fn remove(&mut self, number: u32) -> Result<bool, Failure> {
        if self.frames.is_empty() {
            return Ok(false);
        }
        let index = index_of(number).clamp(1, self.frames.len() + 1) - 1;
        let id = self.all().nth(index).map_or(0, |frame| frame.id);
        self.detach(id)?;
        let stamp = self.revision + 1;
        if index == 0 {
            let next = &self.frames[0];
            let data = match &next.content {
                Content::Whole(data) => data.clone(),
                Content::Sent(_) => self.whole(self.coalesce(next, 0)?),
            };
            let next = self.frames.remove(0);
            self.root = Frame {
                id: next.id,
                stamp,
                gap: next.gap,
                content: Content::Whole(data),
            };
        } else {
            self.frames.remove(index - 1);
        }
        if self.current > index {
            self.current -= 1;
        }
        self.current = self.current.min(self.frames.len());
        self.revision = stamp;
        Ok(true)
    }

    pub fn shape(&self) -> Shape {
        Shape {
            revision: self.revision,
            frames: self.all().map(Frame::shape).collect(),
            state: self.state,
            max_loops: self.max_loops,
            current: self.current,
        }
    }

    pub fn steps(&self) -> Vec<Step> {
        let transmitted = Shape {
            revision: 0,
            frames: vec![FrameShape {
                gap: 0,
                ..self.root.shape()
            }],
            state: AnimationState::Stopped,
            max_loops: 0,
            current: 0,
        };
        self.steps_since(&transmitted).unwrap_or_default()
    }

    pub fn steps_since(&self, shape: &Shape) -> Option<Vec<Step>> {
        let frames: Vec<&Frame> = self.all().collect();
        let kept = shape.frames.len();
        if frames.len() < kept
            || frames
                .iter()
                .zip(&shape.frames)
                .any(|(frame, sent)| frame.id != sent.id)
        {
            return None;
        }
        let mut steps = Vec::new();
        for (number, (frame, sent)) in (1..).zip(frames.iter().zip(&shape.frames)) {
            if frame.stamp != sent.stamp {
                let Content::Whole(data) = &frame.content else {
                    return None;
                };
                let spec = FrameSpec {
                    edit: number,
                    replace: true,
                    ..FrameSpec::default()
                };
                steps.push(Step::Frame(spec, data.clone()));
            }
        }
        steps.extend(frames[kept..].iter().map(|frame| self.creation(frame)));
        for (number, (frame, sent)) in (1..).zip(frames.iter().zip(&shape.frames)) {
            if frame.gap != sent.gap {
                steps.push(Step::Animate(AnimationControl {
                    frame: number,
                    gap: wire_gap(frame.gap),
                    ..AnimationControl::default()
                }));
            }
        }
        let control = AnimationControl {
            current: if self.current == shape.current {
                0
            } else {
                u32::try_from(self.current + 1).unwrap_or(u32::MAX)
            },
            state: (self.state != shape.state).then_some(self.state),
            loops: if self.max_loops == shape.max_loops {
                0
            } else {
                self.max_loops.saturating_add(1)
            },
            ..AnimationControl::default()
        };
        if control != AnimationControl::default() {
            steps.push(Step::Animate(control));
        }
        Some(steps)
    }

    fn creation(&self, frame: &Frame) -> Step {
        let gap = wire_gap(frame.gap);
        match &frame.content {
            Content::Whole(data) => Step::Frame(
                FrameSpec {
                    replace: true,
                    gap,
                    ..FrameSpec::default()
                },
                data.clone(),
            ),
            Content::Sent(sent) => Step::Frame(
                FrameSpec {
                    edit: 0,
                    base: self.number_of(sent.base),
                    x: sent.x,
                    y: sent.y,
                    background: sent.background,
                    replace: sent.replace,
                    gap,
                },
                sent.data.clone(),
            ),
        }
    }

    fn created(&self, spec: FrameSpec, data: ImageData) -> Result<Content, Failure> {
        let sent = Sent {
            base: 0,
            x: spec.x,
            y: spec.y,
            background: spec.background,
            replace: spec.replace,
            data,
        };
        if spec.base == 0 {
            return Ok(Content::Sent(sent));
        }
        let base = self
            .numbered(index_of(spec.base))
            .ok_or_else(|| no_frame(index_of(spec.base)))?;
        if !self.chain_is_long(base) {
            return Ok(Content::Sent(Sent {
                base: base.id,
                ..sent
            }));
        }
        let mut canvas = self.coalesce(base, 0)?;
        canvas.paste(
            self.size(),
            &unpack(&sent.data)?,
            (sent.data.width, sent.data.height),
            (sent.x, sent.y),
            sent.replace,
        );
        Ok(Content::Whole(self.whole(canvas)))
    }

    fn chain_is_long(&self, frame: &Frame) -> bool {
        if frame.base() == 0 {
            return false;
        }
        let limit = 2 * self.area(&self.root);
        let mut area = self.area(frame);
        let mut links = 1;
        let mut link = frame;
        while area < limit && links < LONG_CHAIN {
            let Some(base) = self.by_id(link.base()) else {
                break;
            };
            link = base;
            area += self.area(link);
            links += 1;
        }
        links >= LONG_CHAIN || area >= limit
    }

    fn area(&self, frame: &Frame) -> u64 {
        match &frame.content {
            Content::Whole(_) => u64::from(self.width) * u64::from(self.height),
            Content::Sent(sent) => u64::from(sent.data.width) * u64::from(sent.data.height),
        }
    }

    fn coalesce(&self, frame: &Frame, depth: usize) -> Result<Pixels, Failure> {
        let sent = match &frame.content {
            Content::Whole(data) => return unpack(data),
            Content::Sent(sent) => sent,
        };
        if depth > MAX_DEPTH {
            return Err(Failure::new(
                Code::Einval,
                "the frame is built on too many other frames",
            ));
        }
        let over = unpack(&sent.data)?;
        let covers = sent.x == 0
            && sent.y == 0
            && (sent.data.width, sent.data.height) == (self.width, self.height);
        let mut under = match sent.base {
            0 if covers => return Ok(over),
            0 => self.blank(sent.background, over.opaque)?,
            base => {
                let base = self
                    .by_id(base)
                    .ok_or_else(|| Failure::new(Code::Einval, "the frame's base frame is gone"))?;
                self.coalesce(base, depth + 1)?
            }
        };
        under.paste(
            self.size(),
            &over,
            (sent.data.width, sent.data.height),
            (sent.x, sent.y),
            sent.replace,
        );
        Ok(under)
    }

    fn detach(&mut self, id: u32) -> Result<(), Failure> {
        for at in 0..self.frames.len() {
            if self.frames[at].base() == id {
                let whole = self.whole(self.coalesce(&self.frames[at], 0)?);
                self.frames[at].content = Content::Whole(whole);
            }
        }
        Ok(())
    }

    fn blank(&self, background: u32, opaque: bool) -> Result<Pixels, Failure> {
        let pixel = background.to_be_bytes();
        let pixel = if opaque { &pixel[..RGB] } else { &pixel[..] };
        let len = u64::from(self.width) * u64::from(self.height) * pixel.len() as u64;
        if len > MAX_CANVAS_LEN {
            return Err(Failure::new(
                Code::Efbig,
                "the image is too large to draw its frames",
            ));
        }
        let count = self.width as usize * self.height as usize;
        Ok(Pixels {
            opaque,
            data: pixel.repeat(count),
        })
    }

    fn whole(&self, pixels: Pixels) -> ImageData {
        ImageData {
            key: self.root.data().key,
            width: self.width,
            height: self.height,
            format: if pixels.opaque {
                ImageFormat::Rgb24
            } else {
                ImageFormat::Rgba32
            },
            compressed: false,
            decoded_len: pixels.data.len(),
            bytes: pixels.data.into(),
        }
    }

    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn all(&self) -> impl Iterator<Item = &Frame> {
        iter::once(&self.root).chain(&self.frames)
    }

    fn numbered(&self, number: usize) -> Option<&Frame> {
        match number {
            0 => None,
            1 => Some(&self.root),
            number => self.frames.get(number - 2),
        }
    }

    fn numbered_mut(&mut self, number: usize) -> Option<&mut Frame> {
        match number {
            0 => None,
            1 => Some(&mut self.root),
            number => self.frames.get_mut(number - 2),
        }
    }

    fn by_id(&self, id: u32) -> Option<&Frame> {
        self.all().find(|frame| frame.id == id)
    }

    fn number_of(&self, id: u32) -> u32 {
        (1..)
            .zip(self.all())
            .find(|(_, frame)| frame.id == id)
            .map_or(0, |(number, _)| number)
    }
}

impl Pixels {
    fn bpp(&self) -> usize {
        if self.opaque {
            RGB
        } else {
            RGBA
        }
    }

    fn paste(
        &mut self,
        size: (u32, u32),
        over: &Pixels,
        over_size: (u32, u32),
        at: (u32, u32),
        replace: bool,
    ) {
        let (under_px, over_px) = (self.bpp(), over.bpp());
        let blend = !replace && !over.opaque;
        let [width, height, over_width, over_height, x, y] =
            [size.0, size.1, over_size.0, over_size.1, at.0, at.1].map(|value| value as usize);
        let run = width.saturating_sub(x).min(over_width);
        for row in 0..over_height.min(height.saturating_sub(y)) {
            let under_at = ((row + y) * width + x) * under_px;
            let over_at = row * over_width * over_px;
            let (Some(under), Some(over)) = (
                self.data.get_mut(under_at..under_at + run * under_px),
                over.data.get(over_at..over_at + run * over_px),
            ) else {
                break;
            };
            mix(under, under_px, over, over_px, blend);
        }
    }

    fn copy_rect(
        &mut self,
        stride: u32,
        over: &Pixels,
        size: (u32, u32),
        from: (u32, u32),
        to: (u32, u32),
        replace: bool,
    ) {
        let (under_px, over_px) = (self.bpp(), over.bpp());
        let blend = !replace && !over.opaque;
        let [stride, width, height, from_x, from_y, to_x, to_y] =
            [stride, size.0, size.1, from.0, from.1, to.0, to.1].map(|value| value as usize);
        for row in 0..height {
            let under_at = ((row + to_y) * stride + to_x) * under_px;
            let over_at = ((row + from_y) * stride + from_x) * over_px;
            let (Some(under), Some(over)) = (
                self.data.get_mut(under_at..under_at + width * under_px),
                over.data.get(over_at..over_at + width * over_px),
            ) else {
                break;
            };
            mix(under, under_px, over, over_px, blend);
        }
    }
}

fn mix(under: &mut [u8], under_px: usize, over: &[u8], over_px: usize, blend: bool) {
    for (under, over) in under
        .chunks_exact_mut(under_px)
        .zip(over.chunks_exact(over_px))
    {
        match (blend && over_px == RGBA, under_px) {
            (true, RGBA) => blend_straight(under, over),
            (true, _) => blend_opaque(under, over),
            (false, _) => {
                under[..RGB].copy_from_slice(&over[..RGB]);
                if under_px == RGBA {
                    under[3] = if over_px == RGBA { over[3] } else { u8::MAX };
                }
            }
        }
    }
}

fn blend_opaque(under: &mut [u8], over: &[u8]) {
    let alpha = u32::from(over[3]);
    for channel in 0..RGB {
        let mixed = u32::from(over[channel]) * alpha + u32::from(under[channel]) * (255 - alpha);
        under[channel] = div255_round(mixed) as u8;
    }
}

fn blend_straight(under: &mut [u8], over: &[u8]) {
    let (alpha, under_alpha) = (u32::from(over[3]), u32::from(under[3]));
    let inverse = 255 - alpha;
    let total = alpha * 255 + under_alpha * inverse;
    if total == 0 {
        return;
    }
    for channel in 0..RGB {
        let weighted = u32::from(over[channel]) * alpha * 255
            + u32::from(under[channel]) * under_alpha * inverse;
        under[channel] = (weighted as f32 / total as f32).round_ties_even() as u8;
    }
    under[3] = div255_round(total) as u8;
}

fn div255_round(value: u32) -> u32 {
    let value = value + 128;
    (value + (value >> 8)) >> 8
}

fn unpack(data: &ImageData) -> Result<Pixels, Failure> {
    let pixels = derive::unpack(data).map_err(|error| {
        Failure::new(Code::Einval, format!("can't decode the frame: {error:#}"))
    })?;
    Ok(Pixels {
        opaque: data.format == ImageFormat::Rgb24,
        data: pixels,
    })
}

fn gap_of(gap: i32) -> Option<u32> {
    match gap {
        0 => None,
        gap => Some(u32::try_from(gap).unwrap_or(0)),
    }
}

fn wire_gap(gap: u32) -> i32 {
    match gap {
        0 => -1,
        gap => i32::try_from(gap).unwrap_or(i32::MAX),
    }
}

fn index_of(number: u32) -> usize {
    usize::try_from(number).unwrap_or(usize::MAX)
}

fn no_frame(number: usize) -> Failure {
    Failure::new(
        Code::Einval,
        format!("no frame with number: {number} found"),
    )
}

#[cfg(test)]
mod tests {
    use png::{BitDepth, ColorType, Encoder};

    use super::super::command::Command;
    use super::super::store::ImageKey;
    use super::*;

    const RED: [u8; 4] = [255, 0, 0, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];
    const CLEAR: [u8; 4] = [0; 4];

    fn image(format: ImageFormat, width: u32, height: u32, bytes: Vec<u8>) -> ImageData {
        ImageData {
            key: ImageKey(9),
            width,
            height,
            format,
            compressed: false,
            decoded_len: bytes.len(),
            bytes: bytes.into(),
        }
    }

    fn rgba(width: u32, height: u32, pixels: &[[u8; 4]]) -> ImageData {
        image(ImageFormat::Rgba32, width, height, pixels.concat())
    }

    fn filled(width: u32, height: u32, pixel: [u8; 4]) -> ImageData {
        rgba(width, height, &vec![pixel; (width * height) as usize])
    }

    fn animation() -> Animation {
        Animation::new(filled(2, 2, RED))
    }

    fn frame(keys: &str) -> FrameSpec {
        Command::parse(format!("a=f,{keys}").as_bytes())
            .unwrap()
            .frame()
    }

    fn control(keys: &str) -> AnimationControl {
        Command::parse(format!("a=a,{keys}").as_bytes())
            .unwrap()
            .control()
    }

    fn composition(keys: &str) -> Composition {
        Command::parse(format!("a=c,{keys}").as_bytes())
            .unwrap()
            .composition()
    }

    fn pixels(animation: &Animation, number: usize) -> Pixels {
        let frame = animation.numbered(number).unwrap();
        animation.coalesce(frame, 0).unwrap()
    }

    fn see_through(pixels: &[[u8; 4]]) -> Pixels {
        Pixels {
            opaque: false,
            data: pixels.concat(),
        }
    }

    fn opaque(pixels: &[[u8; 3]]) -> Pixels {
        Pixels {
            opaque: true,
            data: pixels.concat(),
        }
    }

    fn whole(animation: &Animation, number: usize) -> bool {
        matches!(
            animation.numbered(number).unwrap().content,
            Content::Whole(_)
        )
    }

    fn gaps(animation: &Animation) -> Vec<u32> {
        animation.all().map(|frame| frame.gap).collect()
    }

    #[test]
    fn new_frames_are_numbered_after_the_last_and_keep_their_gaps() {
        let mut animation = animation();
        assert_eq!(animation.add(frame(""), filled(2, 2, GREEN)), Ok(2));
        assert_eq!(animation.add(frame("z=-1"), filled(2, 2, BLUE)), Ok(3));
        assert_eq!(animation.add(frame("z=70,r=9"), filled(1, 1, BLUE)), Ok(4));
        assert_eq!(animation.add(frame("r=5"), filled(1, 1, BLUE)), Ok(5));
        assert_eq!(gaps(&animation), [0, 40, 0, 70, 40]);
        assert_eq!(animation.revision(), 4);
    }

    #[test]
    fn a_frame_larger_than_the_image_or_on_a_missing_base_is_refused() {
        let mut animation = animation();
        assert_eq!(
            animation.add(frame(""), filled(3, 2, GREEN)),
            Err(Failure::new(
                Code::Einval,
                "frame width 3 larger than image width: 2"
            ))
        );
        assert_eq!(
            animation.add(frame(""), filled(2, 3, GREEN)),
            Err(Failure::new(
                Code::Einval,
                "frame height 3 larger than image height: 2"
            ))
        );
        assert_eq!(
            animation.add(frame("c=2"), filled(1, 1, GREEN)),
            Err(Failure::new(Code::Einval, "no frame with number: 2 found"))
        );
        assert_eq!(animation, self::animation());
    }

    #[test]
    fn a_partial_frame_is_drawn_on_its_background_or_its_base() {
        let mut animation = animation();
        animation
            .add(frame("x=1,y=1,Y=65535"), filled(1, 1, GREEN))
            .unwrap();
        animation
            .add(frame("c=1,x=1"), filled(1, 1, GREEN))
            .unwrap();
        animation.add(frame(""), filled(1, 1, GREEN)).unwrap();
        animation.add(frame(""), filled(2, 2, BLUE)).unwrap();
        animation
            .add(frame("x=1,y=1"), filled(2, 2, GREEN))
            .unwrap();
        assert_eq!(pixels(&animation, 1), see_through(&[RED; 4]));
        assert_eq!(
            pixels(&animation, 2),
            see_through(&[BLUE, BLUE, BLUE, GREEN])
        );
        assert_eq!(pixels(&animation, 3), see_through(&[RED, GREEN, RED, RED]));
        assert_eq!(
            pixels(&animation, 4),
            see_through(&[GREEN, CLEAR, CLEAR, CLEAR])
        );
        assert_eq!(pixels(&animation, 5), see_through(&[BLUE; 4]));
        assert_eq!(
            pixels(&animation, 6),
            see_through(&[CLEAR, CLEAR, CLEAR, GREEN])
        );
        assert!((2..=6).all(|number| !whole(&animation, number)));
    }

    #[test]
    fn frames_blend_as_kitty_blends_them() {
        let half_red = rgba(1, 1, &[[255, 0, 0, 128]]);
        let mut with_alpha = Animation::new(rgba(1, 1, &[BLUE]));
        with_alpha.add(frame("c=1"), half_red.clone()).unwrap();
        with_alpha.add(frame("c=1,X=1"), half_red.clone()).unwrap();
        with_alpha.add(frame("c=1,X=2"), half_red.clone()).unwrap();
        assert_eq!(pixels(&with_alpha, 2), see_through(&[[128, 0, 127, 255]]));
        assert_eq!(pixels(&with_alpha, 3), see_through(&[[255, 0, 0, 128]]));
        assert_eq!(pixels(&with_alpha, 4), see_through(&[[128, 0, 127, 255]]));

        let mut solid = Animation::new(image(ImageFormat::Rgb24, 1, 1, vec![0, 0, 255]));
        solid.add(frame("c=1"), half_red.clone()).unwrap();
        solid.add(frame("c=1,X=1"), half_red).unwrap();
        solid
            .add(frame("c=1"), image(ImageFormat::Rgb24, 1, 1, vec![0, 9, 0]))
            .unwrap();
        assert_eq!(pixels(&solid, 1), opaque(&[[0, 0, 255]]));
        assert_eq!(pixels(&solid, 2), opaque(&[[128, 0, 127]]));
        assert_eq!(pixels(&solid, 3), opaque(&[[255, 0, 0]]));
        assert_eq!(pixels(&solid, 4), opaque(&[[0, 9, 0]]));

        let mut empty = Animation::new(rgba(1, 1, &[CLEAR]));
        empty.add(frame("c=1"), rgba(1, 1, &[CLEAR])).unwrap();
        empty
            .add(frame("c=1"), rgba(1, 1, &[[0, 0, 255, 0]]))
            .unwrap();
        assert_eq!(pixels(&empty, 2), see_through(&[CLEAR]));
        assert_eq!(pixels(&empty, 3), see_through(&[CLEAR]));
    }

    #[test]
    fn editing_a_frame_draws_onto_it_and_makes_it_whole() {
        let mut animation = animation();
        animation
            .add(frame("c=1,x=1"), filled(1, 2, GREEN))
            .unwrap();
        assert_eq!(
            animation.add(frame("r=2,y=1,X=1"), filled(2, 1, BLUE)),
            Ok(2)
        );
        assert_eq!(
            pixels(&animation, 2),
            see_through(&[RED, GREEN, BLUE, BLUE])
        );
        assert!(whole(&animation, 2));
        assert_eq!(gaps(&animation), [0, 40]);
        animation
            .add(frame("r=2,z=-1"), filled(1, 1, CLEAR))
            .unwrap();
        assert_eq!(gaps(&animation), [0, 0]);
        assert_eq!(
            pixels(&animation, 2),
            see_through(&[RED, GREEN, BLUE, BLUE])
        );

        let before = animation.stored_len();
        assert_eq!(animation.add(frame("r=1,X=1"), filled(1, 1, BLUE)), Ok(1));
        assert_eq!(pixels(&animation, 1), see_through(&[BLUE, RED, RED, RED]));
        assert_eq!(animation.root().format, ImageFormat::Rgba32);
        assert_eq!(animation.stored_len(), before + 16);
        assert_eq!(animation.revision(), 4);
    }

    #[test]
    fn a_frame_on_a_long_chain_of_bases_is_flattened() {
        let mut chain = animation();
        chain.add(frame("c=1"), filled(1, 1, GREEN)).unwrap();
        for base in 2..=5 {
            chain
                .add(frame(&format!("c={base},x=1")), filled(1, 1, BLUE))
                .unwrap();
        }
        assert!((2..=5).all(|number| !whole(&chain, number)));
        assert!(whole(&chain, 6));
        assert_eq!(pixels(&chain, 6), see_through(&[GREEN, BLUE, RED, RED]));

        let mut large = animation();
        large.add(frame("c=1"), filled(2, 2, GREEN)).unwrap();
        large.add(frame("c=2,y=1"), filled(1, 1, BLUE)).unwrap();
        assert!(!whole(&large, 2));
        assert!(whole(&large, 3));
        assert_eq!(pixels(&large, 3), see_through(&[GREEN, GREEN, BLUE, GREEN]));
    }

    #[test]
    fn control_sets_gaps_the_current_frame_the_state_and_the_loops() {
        let mut animation = animation();
        animation.add(frame(""), filled(2, 2, GREEN)).unwrap();
        animation.add(frame(""), filled(2, 2, BLUE)).unwrap();
        let revision = animation.revision();

        animation.control(control("r=1,z=50"));
        animation.control(control("r=3,z=-1"));
        assert_eq!(gaps(&animation), [50, 40, 0]);
        animation.control(control("r=9,z=10"));
        animation.control(control("r=2"));
        animation.control(control("c=9"));
        animation.control(control("v=0,s=0,c=0"));
        assert_eq!(animation.revision(), revision + 2);

        animation.control(control("c=3"));
        assert_eq!(animation.current, 2);
        animation.control(control("s=3,v=4"));
        assert_eq!(
            (animation.state, animation.max_loops),
            (AnimationState::Running, 3)
        );
        assert_eq!(animation.revision(), revision + 4);
        animation.control(control("s=3,v=4,c=3"));
        assert_eq!(animation.revision(), revision + 4);
        animation.control(control("s=1,v=1"));
        assert_eq!(
            (animation.state, animation.max_loops),
            (AnimationState::Stopped, 0)
        );
    }

    #[test]
    fn compose_copies_a_rectangle_from_one_frame_onto_another() {
        let mut animation = Animation::new(rgba(2, 2, &[RED, GREEN, BLUE, RED]));
        animation.add(frame(""), filled(2, 2, BLUE)).unwrap();
        assert_eq!(
            animation.compose(composition("r=1,c=2,w=1,h=1,X=1,Y=0,x=0,y=1,C=1")),
            Ok(())
        );
        assert_eq!(
            pixels(&animation, 2),
            see_through(&[BLUE, BLUE, GREEN, BLUE])
        );
        assert!(whole(&animation, 2));
        animation.compose(composition("r=2,c=1,h=1")).unwrap();
        assert_eq!(pixels(&animation, 1), see_through(&[BLUE, BLUE, BLUE, RED]));
        animation
            .compose(composition("r=1,c=1,w=1,h=1,X=0,x=1,y=1"))
            .unwrap();
        assert_eq!(
            pixels(&animation, 1),
            see_through(&[BLUE, BLUE, BLUE, BLUE])
        );
        assert_eq!(animation.revision(), 4);

        let refused = |keys: &str| animation.clone().compose(composition(keys)).unwrap_err();
        assert_eq!(
            refused("r=3,c=1"),
            Failure::new(Code::Enoent, "no source frame number 3 exists")
        );
        assert_eq!(
            refused("r=1"),
            Failure::new(Code::Enoent, "no destination frame number 0 exists")
        );
        assert_eq!(
            refused("r=1,c=2,w=2,x=1").message,
            "the destination rectangle is out of bounds"
        );
        assert_eq!(
            refused("r=1,c=2,h=1,Y=2").message,
            "the source rectangle is out of bounds"
        );
        let overlap = refused("r=2,c=2,w=2,h=1,y=0,Y=0");
        assert_eq!(overlap.code, Code::Einval);
        assert!(overlap.message.contains("overlap"), "{overlap:?}");
    }

    #[test]
    fn removing_a_frame_keeps_the_others_as_they_look() {
        let mut animation = animation();
        animation
            .add(frame("c=1,x=1"), filled(1, 2, GREEN))
            .unwrap();
        animation.add(frame(""), filled(2, 2, BLUE)).unwrap();
        animation
            .add(frame("c=3,y=1,X=1"), filled(2, 1, RED))
            .unwrap();
        animation.control(control("c=4,r=4,z=25"));
        assert_eq!(animation.remove(3), Ok(true));
        assert_eq!(gaps(&animation), [0, 40, 25]);
        assert!(whole(&animation, 3));
        assert_eq!(pixels(&animation, 3), see_through(&[BLUE, BLUE, RED, RED]));
        assert_eq!(animation.current, 2);

        assert_eq!(animation.remove(1), Ok(true));
        assert_eq!(gaps(&animation), [40, 25]);
        assert_eq!(
            pixels(&animation, 1),
            see_through(&[RED, GREEN, RED, GREEN])
        );
        assert_eq!(animation.current, 1);
        assert_eq!(animation.remove(0), Ok(true));
        assert_eq!(pixels(&animation, 1), see_through(&[BLUE, BLUE, RED, RED]));
        assert_eq!(animation.current, 0);
        assert_eq!(animation.remove(1), Ok(false));
        assert_eq!(animation.root().width, 2);
    }

    #[test]
    fn the_steps_send_every_frame_after_the_root_and_then_the_state() {
        assert!(animation().steps().is_empty());
        let mut animation = animation();
        let green = filled(1, 2, GREEN);
        let blue = filled(1, 1, BLUE);
        animation
            .add(frame("c=1,x=1,z=100"), green.clone())
            .unwrap();
        animation
            .add(frame("Y=255,z=-1,X=1"), blue.clone())
            .unwrap();
        animation.control(control("r=1,z=30"));
        animation.control(control("s=3,v=1,c=2"));
        assert_eq!(
            animation.steps(),
            [
                Step::Frame(
                    FrameSpec {
                        base: 1,
                        x: 1,
                        gap: 100,
                        ..FrameSpec::default()
                    },
                    green
                ),
                Step::Frame(
                    FrameSpec {
                        background: 255,
                        replace: true,
                        gap: -1,
                        ..FrameSpec::default()
                    },
                    blue
                ),
                Step::Animate(AnimationControl {
                    frame: 1,
                    gap: 30,
                    ..AnimationControl::default()
                }),
                Step::Animate(AnimationControl {
                    current: 2,
                    state: Some(AnimationState::Running),
                    ..AnimationControl::default()
                }),
            ]
        );
    }

    #[test]
    fn a_flattened_frame_is_sent_whole() {
        let mut animation = animation();
        animation.add(frame("c=1"), filled(2, 2, GREEN)).unwrap();
        animation.add(frame("c=2,y=1"), filled(1, 1, BLUE)).unwrap();
        let steps = animation.steps();
        let Step::Frame(spec, data) = &steps[1] else {
            panic!("expected a frame, got {steps:?}");
        };
        assert_eq!(
            *spec,
            FrameSpec {
                replace: true,
                gap: 40,
                ..FrameSpec::default()
            }
        );
        assert_eq!(
            (data.width, data.height, data.format, data.compressed),
            (2, 2, ImageFormat::Rgba32, false)
        );
        assert_eq!(*data.bytes, [GREEN, GREEN, BLUE, GREEN].concat());
    }

    #[test]
    fn steps_since_a_shape_bring_a_client_up_to_date() {
        let mut animation = animation();
        animation.add(frame(""), filled(2, 2, GREEN)).unwrap();
        let shape = animation.shape();
        assert_eq!(shape.revision(), 1);
        assert_eq!(animation.steps_since(&shape), Some(Vec::new()));

        let blue = filled(2, 2, BLUE);
        animation.add(frame(""), blue.clone()).unwrap();
        animation.add(frame("r=2,X=1"), filled(1, 1, RED)).unwrap();
        animation.control(control("r=2,z=90"));
        animation.control(control("c=3,s=2"));
        let edited = animation.numbered(2).unwrap().data().clone();
        assert_eq!(*edited.bytes, [RED, GREEN, GREEN, GREEN].concat());
        assert_eq!(
            animation.steps_since(&shape),
            Some(vec![
                Step::Frame(
                    FrameSpec {
                        edit: 2,
                        replace: true,
                        ..FrameSpec::default()
                    },
                    edited
                ),
                Step::Frame(
                    FrameSpec {
                        gap: 40,
                        ..FrameSpec::default()
                    },
                    blue
                ),
                Step::Animate(AnimationControl {
                    frame: 2,
                    gap: 90,
                    ..AnimationControl::default()
                }),
                Step::Animate(AnimationControl {
                    current: 3,
                    state: Some(AnimationState::Loading),
                    ..AnimationControl::default()
                }),
            ])
        );

        let shape = animation.shape();
        animation.control(control("c=1,v=3"));
        assert_eq!(
            animation.steps_since(&shape),
            Some(vec![Step::Animate(AnimationControl {
                current: 1,
                loops: 3,
                ..AnimationControl::default()
            })])
        );
        animation.remove(2).unwrap();
        assert_eq!(animation.steps_since(&shape), None);
    }

    #[test]
    fn the_stored_length_counts_the_frames_and_a_changed_root() {
        let mut animation = animation();
        assert_eq!(animation.stored_len(), 0);
        animation.add(frame(""), filled(2, 2, GREEN)).unwrap();
        let frame_len = 16 + mem::size_of::<Frame>();
        assert_eq!(animation.stored_len(), frame_len);
        animation.add(frame("r=1,X=1"), filled(1, 1, BLUE)).unwrap();
        assert_eq!(animation.stored_len(), frame_len + 16);
        animation.control(control("s=3"));
        assert_eq!(animation.stored_len(), frame_len + 16);
    }

    #[test]
    fn png_frames_are_decoded_for_editing() {
        let mut png = Vec::new();
        let mut encoder = Encoder::new(&mut png, 1, 1);
        encoder.set_color(ColorType::Rgb);
        encoder.set_depth(BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[0, 255, 0]).unwrap();
        writer.finish().unwrap();
        let png = image(ImageFormat::Png, 1, 1, png);
        let mut animation = Animation::new(png.clone());
        animation.add(frame(""), png).unwrap();
        animation.add(frame("r=2"), rgba(1, 1, &[CLEAR])).unwrap();
        assert_eq!(pixels(&animation, 1), see_through(&[GREEN]));
        assert_eq!(pixels(&animation, 2), see_through(&[GREEN]));
        assert!(whole(&animation, 2));
    }
}
