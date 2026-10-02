use std::collections::BTreeMap;

use anyhow::{anyhow, Result};
use tracing::{debug, warn};
use vt100::{MouseProtocolEncoding, MouseProtocolMode};

use super::copy::{CopyMode, Outcome, Snapshot};
use super::graphics::place::Placements;
use super::layout::{Layout, PaneId, Rect, Side, SplitDirection};
use super::mouse::{MouseEvent, Wheel};
use super::pane::Pane;
use super::paste;
use super::render::{self, CopyView, Frame, InputModes, Screens, Viewer};
use crate::keys::KeyDecoder;
use crate::protocol::{ClientTerminal, Direction, Size, Split, WindowSummary};
use crate::settings::{CopyAction, MouseSettings, Settings, Table};

#[derive(Debug, Default)]
pub struct Handled {
    pub redraw: bool,
    pub copied: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct Drag {
    pane: PaneId,
    row: u16,
    col: u16,
    moved: bool,
}

pub struct Window {
    index: usize,
    name: String,
    layout: Layout,
    panes: BTreeMap<PaneId, Pane>,
    active: PaneId,
    copy: BTreeMap<PaneId, CopyMode>,
    drag: Option<Drag>,
}

impl Window {
    pub fn new(index: usize, name: String, id: PaneId, pane: Pane) -> Self {
        Self {
            index,
            name,
            layout: Layout::new(id),
            panes: BTreeMap::from([(id, pane)]),
            active: id,
            copy: BTreeMap::new(),
            drag: None,
        }
    }

    pub fn index(&self) -> usize {
        self.index
    }

    pub fn rename(&mut self, name: String) {
        self.name = name;
    }

    pub fn summary(&self) -> WindowSummary {
        WindowSummary {
            index: self.index,
            name: self.name.clone(),
            panes: self.panes.len(),
        }
    }

    pub fn contains(&self, pane: PaneId) -> bool {
        self.panes.contains_key(&pane)
    }

    pub fn is_empty(&self) -> bool {
        self.layout.is_empty()
    }

    pub fn split(
        &mut self,
        new: PaneId,
        split: Split,
        size: Size,
        spawn: impl FnOnce(Size) -> Result<Pane>,
    ) -> Result<()> {
        let mut layout = self.layout.clone();
        layout.split(self.active, new, split_direction(split), size)?;
        let rect =
            pane_rect(&layout, new, size).ok_or_else(|| anyhow!("pane {new} was not placed"))?;
        let pane = spawn(rect_size(rect))?;
        self.layout = layout;
        self.panes.insert(new, pane);
        self.active = new;
        self.resize(size);
        Ok(())
    }

    pub fn remove(&mut self, pane: PaneId, size: Size) -> Option<Pane> {
        let position = self.layout.panes().iter().position(|id| *id == pane)?;
        self.layout.remove(pane);
        self.copy.remove(&pane);
        if self.drag.is_some_and(|drag| drag.pane == pane) {
            self.drag = None;
        }
        let removed = self.panes.remove(&pane);
        if self.active == pane {
            if let Some(&next) = self.layout.panes().get(position.saturating_sub(1)) {
                self.active = next;
            }
        }
        self.resize(size);
        removed
    }

    pub fn resize(&mut self, size: Size) {
        for (id, rect) in self.layout.rects(size) {
            let Some(pane) = self.panes.get(&id) else {
                continue;
            };
            if let Err(err) = pane.resize(rect_size(rect)) {
                warn!(pane = %id, "resizing the pane failed: {err:#}");
            }
            if let Some(copy) = self.copy.get_mut(&id) {
                copy.resize(rect_size(rect));
            }
        }
    }

    pub fn set_client_terminal(&self, terminal: ClientTerminal) {
        for (id, pane) in &self.panes {
            if let Err(err) = pane.set_cell_pixels(terminal.cell_pixels) {
                warn!(pane = %id, "setting the pixel size failed: {err:#}");
            }
            pane.set_graphics(terminal.graphics);
        }
    }

    pub fn compose(&self, size: Size, settings: &Settings, viewer: Viewer<'_>) -> Frame {
        let mut frame = render::compose(&self.layout, size, self.active, self, settings, viewer);
        let clicks = self.panes.len() > 1 || self.copy.contains_key(&self.active);
        report_mouse(&mut frame.modes, settings.mouse.scroll, clicks);
        frame
    }

    pub fn active_pane(&self) -> PaneId {
        self.active
    }

    pub fn shows_output_of(&self, pane: PaneId) -> bool {
        self.contains(pane) && !self.copy.contains_key(&pane)
    }

    pub fn copy_mode(&mut self, size: Size, page_up: bool) {
        let id = self.active;
        if let Some(copy) = self.copy.get_mut(&id) {
            if page_up {
                copy.page_up();
            }
            return;
        }
        let Some(pane) = self.panes.get(&id) else {
            return;
        };
        let Some(rect) = pane_rect(&self.layout, id, size) else {
            return;
        };
        let mut copy = browse(pane, rect);
        if page_up {
            copy.page_up();
        }
        self.copy.insert(id, copy);
    }

    pub fn keys(
        &mut self,
        keys: &mut KeyDecoder,
        bytes: &[u8],
        timed_out: bool,
        bindings: &Table<CopyAction>,
    ) -> Handled {
        let id = self.active;
        let Some(copy) = self.copy.get_mut(&id) else {
            let mut input = keys.take_pending();
            input.extend_from_slice(bytes);
            if !input.is_empty() {
                self.write_input_to(id, input);
            }
            return Handled::default();
        };
        keys.push(bytes);
        let mut handled = Handled::default();
        while let Some(decoded) = keys
            .next_key()
            .or_else(|| timed_out.then(|| keys.time_out()).flatten())
        {
            handled.redraw = true;
            match copy.press(&decoded, bindings) {
                Outcome::Stay => {}
                Outcome::Refresh => {
                    if let Some(pane) = self.panes.get(&id) {
                        copy.refresh(Snapshot::new(pane.snapshot()));
                    }
                }
                Outcome::Copy(text) => {
                    handled.copied = Some(text);
                    self.leave_copy_mode(id, keys);
                    break;
                }
                Outcome::Exit => {
                    self.leave_copy_mode(id, keys);
                    break;
                }
            }
        }
        handled
    }

    fn leave_copy_mode(&mut self, id: PaneId, keys: &mut KeyDecoder) {
        self.copy.remove(&id);
        let rest = keys.take_pending();
        if !rest.is_empty() {
            self.write_input_to(id, rest);
        }
    }

    pub fn paste(&self, text: &str) {
        let id = self.active;
        let Some(pane) = self.panes.get(&id) else {
            return;
        };
        let bracketed = pane.with_screen(vt100::Screen::bracketed_paste);
        self.write_input_to(id, paste::typed(text, bracketed));
    }

    pub fn pane_at(&self, index: usize) -> Result<PaneId> {
        self.layout
            .panes()
            .get(index)
            .copied()
            .ok_or_else(|| anyhow!("can't find pane {index} in window {}", self.index))
    }

    pub fn focus(&mut self, pane: PaneId) -> bool {
        let changed = self.active != pane && self.contains(pane);
        if changed {
            self.active = pane;
        }
        changed
    }

    pub fn focus_next(&mut self) -> bool {
        let order = self.layout.panes();
        let position = order
            .iter()
            .position(|id| *id == self.active)
            .unwrap_or_default();
        match order.get((position + 1) % order.len().max(1)) {
            Some(&next) => self.focus(next),
            None => false,
        }
    }

    pub fn focus_neighbor(&mut self, direction: Direction, size: Size) -> bool {
        match self.layout.neighbor(self.active, side(direction), size) {
            Some(pane) => self.focus(pane),
            None => false,
        }
    }

    pub fn write_input_to(&self, id: PaneId, bytes: Vec<u8>) {
        let Some(pane) = self.panes.get(&id) else {
            return;
        };
        if let Err(err) = pane.write_input(bytes) {
            debug!(pane = %id, "dropping input: {err:#}");
        }
    }

    pub fn mouse(&mut self, event: MouseEvent, size: Size, settings: &MouseSettings) -> Handled {
        if let Some(drag) = self.drag {
            if event.is_left_drag() || event.is_left_release() {
                return self.continue_drag(drag, event, size, settings);
            }
        }
        let Some((id, rect)) = self
            .layout
            .rects(size)
            .into_iter()
            .find(|(_, rect)| rect.contains(event.row, event.col))
        else {
            return Handled::default();
        };
        let mut redraw = event.is_click() && self.focus(id);
        let (row, col) = (event.row - rect.row, event.col - rect.col);
        if event.is_left_press() {
            self.drag = Some(Drag {
                pane: id,
                row,
                col,
                moved: false,
            });
        }
        if let Some(wheel) = event.wheel() {
            redraw |= self.wheel(id, rect, event, wheel, settings);
        } else if let Some(copy) = self.copy.get_mut(&id) {
            if event.is_left_press() {
                copy.click(row, col);
                redraw = true;
            }
        } else {
            self.forward(id, rect, event);
        }
        Handled {
            redraw,
            copied: None,
        }
    }

    fn wheel(
        &mut self,
        id: PaneId,
        rect: Rect,
        event: MouseEvent,
        wheel: Wheel,
        settings: &MouseSettings,
    ) -> bool {
        let lines = usize::from(settings.scroll_lines);
        if let Some(copy) = self.copy.get_mut(&id) {
            if copy.scroll(wheel, lines) == Outcome::Exit {
                self.copy.remove(&id);
            }
            return true;
        }
        let Some(pane) = self.panes.get(&id) else {
            return false;
        };
        let (mode, alternate, application_cursor) = pane.with_screen(|screen| {
            (
                screen.mouse_protocol_mode(),
                screen.alternate_screen(),
                screen.application_cursor(),
            )
        });
        if event.reported_in(mode) {
            self.forward(id, rect, event);
        } else if alternate {
            self.write_input_to(id, arrows(wheel, application_cursor, lines));
        } else if settings.scroll && wheel == Wheel::Up {
            let mut copy = browse(pane, rect).exit_at_bottom();
            if copy.scroll(wheel, lines) == Outcome::Stay {
                self.copy.insert(id, copy);
                return true;
            }
        }
        false
    }

    fn continue_drag(
        &mut self,
        drag: Drag,
        event: MouseEvent,
        size: Size,
        settings: &MouseSettings,
    ) -> Handled {
        let releasing = event.is_left_release();
        self.drag = (!releasing).then_some(Drag {
            moved: true,
            ..drag
        });
        let Some(rect) = pane_rect(&self.layout, drag.pane, size) else {
            self.drag = None;
            return Handled::default();
        };
        let inside = clamp_into(event, rect);
        let row = i32::from(event.row) - i32::from(rect.row);
        let col = inside.col - rect.col;
        if let Some(copy) = self.copy.get_mut(&drag.pane) {
            if !releasing {
                copy.drag(row, col);
                return Handled {
                    redraw: true,
                    copied: None,
                };
            }
            if !drag.moved {
                return Handled::default();
            }
            let outcome = copy.release();
            self.copy.remove(&drag.pane);
            return Handled {
                redraw: true,
                copied: match outcome {
                    Outcome::Copy(text) => Some(text),
                    _ => None,
                },
            };
        }
        let Some(pane) = self.panes.get(&drag.pane) else {
            return Handled::default();
        };
        if pane.with_screen(vt100::Screen::mouse_protocol_mode) != MouseProtocolMode::None {
            self.forward(drag.pane, rect, inside);
            return Handled::default();
        }
        if releasing || !settings.scroll {
            return Handled::default();
        }
        let mut copy = browse(pane, rect);
        copy.click(drag.row, drag.col);
        copy.drag(row, col);
        self.copy.insert(drag.pane, copy);
        Handled {
            redraw: true,
            copied: None,
        }
    }

    fn forward(&self, id: PaneId, rect: Rect, event: MouseEvent) {
        let Some(pane) = self.panes.get(&id) else {
            return;
        };
        let (mode, encoding) = pane.with_screen(|screen| {
            (
                screen.mouse_protocol_mode(),
                screen.mouse_protocol_encoding(),
            )
        });
        let report = event
            .reported_in(mode)
            .then(|| event.encode_for(rect, encoding))
            .flatten();
        if let Some(report) = report {
            if let Err(err) = pane.write_input(report) {
                debug!(pane = %id, "dropping a mouse report: {err:#}");
            }
        }
    }

    pub fn kill(&self) {
        for pane in self.panes.values() {
            pane.kill();
        }
    }
}

impl Screens for Window {
    fn with_pane<R>(
        &self,
        pane: PaneId,
        read: impl FnOnce(&vt100::Screen, Option<&Placements>) -> R,
    ) -> Option<R> {
        self.panes.get(&pane).map(|pane| pane.with_pane(read))
    }

    fn copy_view(&self, pane: PaneId) -> Option<CopyView<'_>> {
        self.copy.get(&pane).map(CopyMode::view)
    }
}

fn report_mouse(modes: &mut InputModes, scroll: bool, clicks: bool) {
    let least = if scroll {
        MouseProtocolMode::ButtonMotion
    } else if clicks {
        MouseProtocolMode::PressRelease
    } else {
        MouseProtocolMode::None
    };
    if mouse_level(modes.mouse_protocol_mode) < mouse_level(least) {
        modes.mouse_protocol_mode = least;
    }
    if modes.mouse_protocol_mode != MouseProtocolMode::None {
        modes.mouse_protocol_encoding = MouseProtocolEncoding::Sgr;
    }
}

fn mouse_level(mode: MouseProtocolMode) -> u8 {
    match mode {
        MouseProtocolMode::None => 0,
        MouseProtocolMode::Press => 1,
        MouseProtocolMode::PressRelease => 2,
        MouseProtocolMode::ButtonMotion => 3,
        MouseProtocolMode::AnyMotion => 4,
    }
}

fn browse(pane: &Pane, rect: Rect) -> CopyMode {
    CopyMode::new(Snapshot::new(pane.snapshot()), rect_size(rect))
}

fn arrows(wheel: Wheel, application_cursor: bool, count: usize) -> Vec<u8> {
    let introducer = if application_cursor { b'O' } else { b'[' };
    let direction = match wheel {
        Wheel::Up => b'A',
        Wheel::Down => b'B',
    };
    [0x1b, introducer, direction].repeat(count)
}

fn clamp_into(event: MouseEvent, rect: Rect) -> MouseEvent {
    MouseEvent {
        row: event.row.min(rect.bottom().saturating_sub(1)).max(rect.row),
        col: event.col.min(rect.right().saturating_sub(1)).max(rect.col),
        ..event
    }
}

fn pane_rect(layout: &Layout, pane: PaneId, size: Size) -> Option<Rect> {
    layout
        .rects(size)
        .into_iter()
        .find(|(id, _)| *id == pane)
        .map(|(_, rect)| rect)
}

fn split_direction(split: Split) -> SplitDirection {
    match split {
        Split::LeftRight => SplitDirection::LeftRight,
        Split::TopBottom => SplitDirection::TopBottom,
    }
}

fn side(direction: Direction) -> Side {
    match direction {
        Direction::Left => Side::Left,
        Direction::Right => Side::Right,
        Direction::Up => Side::Up,
        Direction::Down => Side::Down,
    }
}

fn rect_size(rect: Rect) -> Size {
    Size {
        rows: rect.rows,
        cols: rect.cols,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modes(mode: MouseProtocolMode, encoding: MouseProtocolEncoding) -> InputModes {
        InputModes {
            mouse_protocol_mode: mode,
            mouse_protocol_encoding: encoding,
            ..InputModes::default()
        }
    }

    #[test]
    fn the_wheel_becomes_arrow_keys_in_the_form_the_pane_asked_for() {
        assert_eq!(arrows(Wheel::Up, false, 3), b"\x1b[A\x1b[A\x1b[A");
        assert_eq!(arrows(Wheel::Down, false, 1), b"\x1b[B");
        assert_eq!(arrows(Wheel::Up, true, 1), b"\x1bOA");
        assert_eq!(arrows(Wheel::Down, true, 2), b"\x1bOB\x1bOB");
    }

    #[test]
    fn a_drag_outside_its_pane_is_held_at_the_pane_edge() {
        let pane = Rect {
            row: 3,
            col: 41,
            rows: 10,
            cols: 39,
        };
        let at = |row, col| MouseEvent {
            code: 32,
            pressed: true,
            row,
            col,
        };
        assert_eq!(clamp_into(at(5, 50), pane), at(5, 50));
        assert_eq!(clamp_into(at(0, 10), pane), at(3, 41));
        assert_eq!(clamp_into(at(20, 90), pane), at(12, 79));
    }

    #[test]
    fn scrolling_forces_button_motion_in_sgr_on_every_window() {
        for mode in [
            MouseProtocolMode::None,
            MouseProtocolMode::Press,
            MouseProtocolMode::PressRelease,
        ] {
            for clicks in [false, true] {
                let mut forced = modes(mode, MouseProtocolEncoding::Default);
                report_mouse(&mut forced, true, clicks);
                assert_eq!(
                    forced,
                    modes(MouseProtocolMode::ButtonMotion, MouseProtocolEncoding::Sgr)
                );
            }
        }
    }

    #[test]
    fn a_pane_asking_for_more_mouse_events_keeps_them() {
        for mode in [
            MouseProtocolMode::ButtonMotion,
            MouseProtocolMode::AnyMotion,
        ] {
            for scroll in [false, true] {
                let mut kept = modes(mode, MouseProtocolEncoding::Utf8);
                report_mouse(&mut kept, scroll, true);
                assert_eq!(kept, modes(mode, MouseProtocolEncoding::Sgr));
            }
        }
        let mut kept = modes(
            MouseProtocolMode::PressRelease,
            MouseProtocolEncoding::Default,
        );
        report_mouse(&mut kept, false, true);
        assert_eq!(
            kept,
            modes(MouseProtocolMode::PressRelease, MouseProtocolEncoding::Sgr)
        );
    }

    #[test]
    fn without_scrolling_several_panes_or_copy_mode_force_click_reports_in_sgr() {
        for mode in [MouseProtocolMode::None, MouseProtocolMode::Press] {
            let mut forced = modes(mode, MouseProtocolEncoding::Default);
            report_mouse(&mut forced, false, true);
            assert_eq!(
                forced,
                modes(MouseProtocolMode::PressRelease, MouseProtocolEncoding::Sgr)
            );
        }
    }

    #[test]
    fn without_scrolling_a_single_live_pane_only_gets_the_mouse_when_it_asks() {
        let mut quiet = modes(MouseProtocolMode::None, MouseProtocolEncoding::Default);
        report_mouse(&mut quiet, false, false);
        assert_eq!(
            quiet,
            modes(MouseProtocolMode::None, MouseProtocolEncoding::Default)
        );

        let mut asking = modes(MouseProtocolMode::Press, MouseProtocolEncoding::Default);
        report_mouse(&mut asking, false, false);
        assert_eq!(
            asking,
            modes(MouseProtocolMode::Press, MouseProtocolEncoding::Sgr)
        );
    }
}
