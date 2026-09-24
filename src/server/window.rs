use std::collections::BTreeMap;

use anyhow::{anyhow, Result};
use tracing::{debug, warn};
use vt100::{MouseProtocolEncoding, MouseProtocolMode};

use super::layout::{Layout, PaneId, Rect, Side, SplitDirection};
use super::mouse::MouseEvent;
use super::pane::Pane;
use super::render::{self, Frame, InputModes, Screens};
use crate::protocol::{Direction, Size, Split, WindowSummary};

pub struct Window {
    index: usize,
    name: String,
    layout: Layout,
    panes: BTreeMap<PaneId, Pane>,
    active: PaneId,
}

impl Window {
    pub fn new(index: usize, name: String, id: PaneId, pane: Pane) -> Self {
        Self {
            index,
            name,
            layout: Layout::new(id),
            panes: BTreeMap::from([(id, pane)]),
            active: id,
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
        let rect = layout
            .rects(size)
            .into_iter()
            .find(|(id, _)| *id == new)
            .map(|(_, rect)| rect)
            .ok_or_else(|| anyhow!("pane {new} was not placed"))?;
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
        let removed = self.panes.remove(&pane);
        if self.active == pane {
            if let Some(&next) = self.layout.panes().get(position.saturating_sub(1)) {
                self.active = next;
            }
        }
        self.resize(size);
        removed
    }

    pub fn resize(&self, size: Size) {
        for (id, rect) in self.layout.rects(size) {
            let Some(pane) = self.panes.get(&id) else {
                continue;
            };
            if let Err(err) = pane.resize(rect_size(rect)) {
                warn!(pane = %id, "resizing the pane failed: {err:#}");
            }
        }
    }

    pub fn compose(&self, size: Size) -> Frame {
        let mut frame = render::compose(&self.layout, size, self.active, self);
        report_clicks(&mut frame.modes, self.panes.len() > 1);
        frame
    }

    pub fn active_pane(&self) -> PaneId {
        self.active
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

    pub fn write_input(&self, bytes: Vec<u8>) {
        let Some(pane) = self.panes.get(&self.active) else {
            return;
        };
        if let Err(err) = pane.write_input(bytes) {
            debug!(pane = %self.active, "dropping input: {err:#}");
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
    fn with_screen<R>(&self, pane: PaneId, read: impl FnOnce(&vt100::Screen) -> R) -> Option<R> {
        self.panes.get(&pane).map(|pane| pane.with_screen(read))
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
