use std::collections::BTreeMap;

use anyhow::{anyhow, Result};
use tracing::{debug, warn};
use vt100::{MouseProtocolEncoding, MouseProtocolMode};

use super::copy::{CopyMode, Outcome, Snapshot};
use super::graphics::place::Placements;
use super::layout::{Layout, PaneId, Rect, Side, SplitDirection};
use super::mouse::MouseEvent;
use super::pane::Pane;
use super::render::{self, CopyView, Frame, InputModes, Screens, Viewer};
use crate::keys::KeyDecoder;
use crate::protocol::{ClientTerminal, Direction, Size, Split, WindowSummary};
use crate::settings::{CopyAction, Settings, Table};

pub struct Window {
    index: usize,
    name: String,
    layout: Layout,
    panes: BTreeMap<PaneId, Pane>,
    active: PaneId,
    copy: BTreeMap<PaneId, CopyMode>,
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
        report_clicks(&mut frame.modes, self.panes.len() > 1);
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
        let mut copy = CopyMode::new(Snapshot::new(pane.snapshot()), rect_size(rect));
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
    ) -> bool {
        let id = self.active;
        let Some(copy) = self.copy.get_mut(&id) else {
            let mut input = keys.take_pending();
            input.extend_from_slice(bytes);
            if !input.is_empty() {
                self.write_input_to(id, input);
            }
            return false;
        };
        keys.push(bytes);
        let mut redraw = false;
        while let Some(decoded) = keys
            .next_key()
            .or_else(|| timed_out.then(|| keys.time_out()).flatten())
        {
            redraw = true;
            match copy.press(&decoded, bindings) {
                Outcome::Stay => {}
                Outcome::Refresh => {
                    if let Some(pane) = self.panes.get(&id) {
                        copy.refresh(Snapshot::new(pane.snapshot()));
                    }
                }
                Outcome::Exit => {
                    self.copy.remove(&id);
                    let rest = keys.take_pending();
                    if !rest.is_empty() {
                        self.write_input_to(id, rest);
                    }
                    break;
                }
            }
        }
        redraw
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

    pub fn mouse(&mut self, event: MouseEvent, size: Size) -> bool {
        let Some((id, rect)) = self
            .layout
            .rects(size)
            .into_iter()
            .find(|(_, rect)| rect.contains(event.row, event.col))
        else {
            return false;
        };
        let focused = event.is_click() && self.focus(id);
        if self.copy.contains_key(&id) {
            return focused;
        }
        if let Some(pane) = self.panes.get(&id) {
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
        focused
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

fn report_clicks(modes: &mut InputModes, several_panes: bool) {
    let reports_presses_only = matches!(
        modes.mouse_protocol_mode,
        MouseProtocolMode::None | MouseProtocolMode::Press
    );
    if several_panes && reports_presses_only {
        modes.mouse_protocol_mode = MouseProtocolMode::PressRelease;
    }
    if modes.mouse_protocol_mode != MouseProtocolMode::None {
        modes.mouse_protocol_encoding = MouseProtocolEncoding::Sgr;
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
    fn several_panes_force_click_reports_in_sgr() {
        for mode in [MouseProtocolMode::None, MouseProtocolMode::Press] {
            let mut forced = modes(mode, MouseProtocolEncoding::Default);
            report_clicks(&mut forced, true);
            assert_eq!(
                forced,
                modes(MouseProtocolMode::PressRelease, MouseProtocolEncoding::Sgr)
            );
        }
    }

    #[test]
    fn a_pane_asking_for_more_mouse_events_keeps_them() {
        for mode in [
            MouseProtocolMode::PressRelease,
            MouseProtocolMode::ButtonMotion,
            MouseProtocolMode::AnyMotion,
        ] {
            let mut kept = modes(mode, MouseProtocolEncoding::Utf8);
            report_clicks(&mut kept, true);
            assert_eq!(kept, modes(mode, MouseProtocolEncoding::Sgr));
        }
    }

    #[test]
    fn a_single_pane_only_gets_the_mouse_when_it_asks() {
        let mut quiet = modes(MouseProtocolMode::None, MouseProtocolEncoding::Default);
        report_clicks(&mut quiet, false);
        assert_eq!(
            quiet,
            modes(MouseProtocolMode::None, MouseProtocolEncoding::Default)
        );

        let mut asking = modes(MouseProtocolMode::Press, MouseProtocolEncoding::Default);
        report_clicks(&mut asking, false);
        assert_eq!(
            asking,
            modes(MouseProtocolMode::Press, MouseProtocolEncoding::Sgr)
        );
    }
}
