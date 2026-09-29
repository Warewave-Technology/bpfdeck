//! The fleet run's compare tab (docs/design-fleet.md, F4): its state and keys. The
//! comparison itself is `model::compare`, computed from the members' runs when drawn.

use std::cell::Cell;

use super::target::Target;
use super::{App, Level, Screen};
use crate::keymap::Action;
use crate::model::compare::{self, Comparison, Member, SortColumn};
use crate::model::run_state::Run;
use crate::msg::Cmd;

/// Rows per key × host table.
pub const TOP_KEYS: usize = 10;

#[derive(Debug, Default)]
pub struct CompareView {
    /// Histograms as one merged histogram instead of a row per host (`m`).
    pub merged: bool,
    /// Tables sorted by the total or by one host's column (`s` cycles).
    pub sort: SortColumn,
    /// Host row under the cursor (`Enter` opens its tab).
    pub cursor: usize,
    /// Scroll offset; clamped by the renderer.
    pub scroll: Cell<u16>,
}

impl App {
    /// The compare tab is selected (and there is a fleet run to compare).
    pub fn compare_selected(&self) -> bool {
        self.compare && self.fleet.is_some()
    }

    /// Members of the fleet run that still exist: their target and current run.
    pub fn fleet_members(&self) -> Vec<(&Target, &Run)> {
        let Some(fleet) = &self.fleet else {
            return Vec::new();
        };
        fleet
            .members
            .iter()
            .filter_map(|(id, run_id)| {
                let t = self.targets.iter().find(|t| t.id == *id)?;
                let run = t.run.as_ref().filter(|r| r.id == *run_id)?;
                Some((t, run))
            })
            .collect()
    }

    pub fn comparison(&self) -> Comparison {
        let members: Vec<Member> = self
            .fleet_members()
            .into_iter()
            .map(|(t, run)| Member { label: &t.label, run })
            .collect();
        compare::compare(&members, self.compare_view.sort, TOP_KEYS)
    }

    pub(super) fn compare_action(&mut self, action: Action) -> Vec<Cmd> {
        let hosts = self.fleet_members().len();
        let view = &mut self.compare_view;
        match action {
            Action::Stop | Action::StopAll => return self.stop_fleet_all(),
            Action::Merge => view.merged = !view.merged,
            Action::ToggleSort => {
                view.sort = match view.sort {
                    None if hosts > 0 => Some(0),
                    Some(i) if i + 1 < hosts => Some(i + 1),
                    _ => None,
                };
            }
            Action::Down => view.cursor = (view.cursor + 1).min(hosts.saturating_sub(1)),
            Action::Up => view.cursor = view.cursor.saturating_sub(1),
            Action::ScrollDown => view.scroll.set(view.scroll.get().saturating_add(10)),
            Action::ScrollUp => view.scroll.set(view.scroll.get().saturating_sub(10)),
            Action::Submit => {
                let id = self
                    .fleet_members()
                    .get(self.compare_view.cursor)
                    .map(|(t, _)| t.id);
                if let Some(i) = id.and_then(|id| self.targets.iter().position(|t| t.id == id)) {
                    self.active = i;
                    self.compare = false;
                }
            }
            Action::PrevTarget => self.switch_target(-1),
            Action::NextTarget => self.switch_target(1),
            Action::ToggleFullWidth => self.full_width = !self.full_width,
            Action::Close => self.screen = Screen::Browser,
            Action::Help => self.overlay = Some(super::Overlay::Help),
            _ => {}
        }
        Vec::new()
    }

    /// Stop every member of the fleet run that is still running.
    pub(super) fn stop_fleet_all(&mut self) -> Vec<Cmd> {
        let Some(fleet) = &self.fleet else {
            return Vec::new();
        };
        let members = fleet.members.clone();
        let mut cmds = Vec::new();
        for (target, run_id) in members {
            if let Some(i) = self
                .targets
                .iter()
                .position(|t| t.id == target && t.run.as_ref().is_some_and(|r| r.id == run_id))
            {
                cmds.extend(self.stop_run(i));
            }
        }
        if !cmds.is_empty() {
            self.notify(Level::Info, format!("stopping on {} targets", cmds.len()));
        }
        cmds
    }
}
