//! A bar's workspaces, as vibepanel shows them: a pill for each workspace with windows and
//! for the one shown, the shown one wider and filled with the accent. A switch animates:
//! every pill moves from its width towards its new one on one shared clock, so the row's
//! width stays the same and the items after it do not move. A workspace added grows out of
//! its gap; one removed shrinks away.

use std::cell::RefCell;
use std::rc::Rc;

use gtk4::{self as gtk, glib, prelude::*};
use shell_core::config::Sizes;
use shell_core::niri::{self, Workspace};
use tokio::sync::mpsc::UnboundedSender;

/// A switch's length, in µs (vibepanel's).
const DURATION: f64 = 200_000.0;

pub struct Workspaces {
    pub row: gtk::Box,
    niri: UnboundedSender<niri::Command>,
    /// A pill's height and width, the shown one's width, and the gap between pills.
    size: i32,
    active: i32,
    gap: i32,
    state: Rc<RefCell<State>>,
}

#[derive(Default)]
struct State {
    pills: Vec<Pill>,
    /// The animation's first frame, in µs of the frame clock; `None` until it is drawn.
    start: Option<i64>,
    ticking: bool,
}

/// A workspace's button, holding the pill drawn. Its width, the pill's and a gap (half
/// either side), goes from `from` to `to`; `to` is 0 for one removed.
struct Pill {
    id: u64,
    button: gtk::Button,
    from: f64,
    to: f64,
    now: f64,
}

impl Workspaces {
    pub fn new(niri: UnboundedSender<niri::Command>, sizes: Sizes) -> Self {
        let size = sizes.icon as i32;
        Self {
            row: gtk::Box::new(gtk::Orientation::Horizontal, 0),
            niri,
            size,
            // vibepanel's, on the owner's 30 px bar: 16 px pills, 22 px wide when shown.
            active: (size * 11 + 4) / 8,
            gap: (sizes.spacing * 3 / 4) as i32,
            state: Rc::default(),
        }
    }

    /// Show `workspaces`, those of this bar's output that have windows or are shown.
    pub fn show(&self, workspaces: &[Workspace]) {
        let mut state = self.state.borrow_mut();
        // Before the bar is drawn, nothing to animate.
        let snap = !self.row.is_mapped();
        let old: Vec<u64> = state.pills.iter().map(|p| p.id).collect();
        let new: Vec<u64> = workspaces.iter().map(|w| w.id).collect();
        let mut pills = std::mem::take(&mut state.pills);
        for id in merge(&old, &new) {
            let pill = match pills.iter().position(|p| p.id == id) {
                Some(i) => pills.remove(i),
                None => self.new_pill(id),
            };
            let workspace = workspaces.iter().find(|w| w.id == id);
            if snap && workspace.is_none() {
                self.row.remove(&pill.button);
                continue;
            }
            let button = &pill.button;
            // Gone: not clickable while it shrinks.
            button.set_can_target(workspace.is_some());
            let to = match workspace {
                Some(w) if w.active => (self.active + self.gap) as f64,
                Some(_) => (self.size + self.gap) as f64,
                None => 0.0,
            };
            if let Some(w) = workspace {
                if w.active {
                    button.add_css_class("active");
                } else {
                    button.remove_css_class("active");
                }
                let label = match &w.name {
                    Some(name) => name.clone(),
                    None => format!("Workspace {}", w.idx),
                };
                button.update_property(&[gtk::accessible::Property::Label(&label)]);
            }
            let now = if snap { to } else { pill.now };
            state.pills.push(Pill {
                from: now,
                to,
                now,
                ..pill
            });
        }
        // The buttons in the pills' order.
        let mut previous: Option<gtk::Widget> = None;
        for pill in &state.pills {
            if pill.button.parent().is_none() {
                self.row.append(&pill.button);
            }
            self.row
                .reorder_child_after(&pill.button, previous.as_ref());
            previous = Some(pill.button.clone().upcast());
        }
        lay_out(&state.pills, self.gap, self.size);
        // From where each one is now, on a clock started again.
        state.start = None;
        if !snap && !state.ticking && state.pills.iter().any(|p| p.now != p.to) {
            state.ticking = true;
            // The row is the callback's argument, not held by it: a row destroyed while
            // it animates is freed, its callback with it.
            let (state, gap, size) = (self.state.clone(), self.gap, self.size);
            self.row.add_tick_callback(move |row, clock| {
                tick(row, &mut state.borrow_mut(), clock.frame_time(), gap, size)
            });
        }
    }

    fn new_pill(&self, id: u64) -> Pill {
        let drawn = gtk::Box::builder()
            .css_classes(["pill"])
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        let button = gtk::Button::builder()
            .css_classes(["workspace"])
            .child(&drawn)
            .focus_on_click(false)
            .build();
        let niri = self.niri.clone();
        button.connect_clicked(move |_| {
            // Fails only once the core has stopped, and the shell with it.
            let _ = niri.send(niri::Command::FocusWorkspace(id));
        });
        Pill {
            id,
            button,
            from: 0.0,
            to: 0.0,
            now: 0.0,
        }
    }
}

/// One frame of the animation; the pills removed go once it ends.
fn tick(row: &gtk::Box, state: &mut State, frame: i64, gap: i32, size: i32) -> glib::ControlFlow {
    let start = *state.start.get_or_insert(frame);
    let t = ((frame - start) as f64 / DURATION).min(1.0);
    for pill in &mut state.pills {
        pill.now = pill.from + (pill.to - pill.from) * t;
    }
    lay_out(&state.pills, gap, size);
    if t < 1.0 {
        return glib::ControlFlow::Continue;
    }
    state.pills.retain(|pill| {
        if pill.to == 0.0 {
            row.remove(&pill.button);
        }
        pill.to != 0.0
    });
    state.ticking = false;
    glib::ControlFlow::Break
}

/// Each button as wide as its pill now is, in whole pixels; the pill drawn is that less
/// the gap.
fn lay_out(pills: &[Pill], gap: i32, size: i32) {
    let widths = whole_pixels(pills.iter().map(|p| p.now));
    for (pill, width) in pills.iter().zip(widths) {
        pill.button.set_size_request(width, -1);
        if let Some(drawn) = pill.button.child() {
            drawn.set_size_request((width - gap).max(0), size);
        }
    }
}

/// The pills' order: `new`'s, with those of `old` not in it each after the one before it
/// in `old` (or first), to shrink where they were.
fn merge(old: &[u64], new: &[u64]) -> Vec<u64> {
    let mut merged = new.to_vec();
    let mut after = None;
    for &id in old {
        if !new.contains(&id) {
            let at = after.and_then(|a| merged.iter().position(|&m| m == a));
            merged.insert(at.map_or(0, |i| i + 1), id);
        }
        after = Some(id);
    }
    merged
}

/// `widths` in whole pixels, each rounded where it ends rather than alone, so that they
/// always add up to their sum rounded: a switch's two widths changing in opposite ways
/// never move what comes after them.
fn whole_pixels(widths: impl Iterator<Item = f64>) -> Vec<i32> {
    let (mut sum, mut end) = (0.0, 0);
    widths
        .map(|width| {
            sum += width;
            let start = end;
            end = sum.round() as i32;
            end - start
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_pixels_add_up_to_the_rounded_sum() {
        assert_eq!(whole_pixels([7.5, 7.5, 7.5].into_iter()), [8, 7, 8]);
        assert_eq!(whole_pixels([0.4, 0.4, 0.4].into_iter()), [0, 1, 0]);
        assert!(whole_pixels([].into_iter()).is_empty());
    }

    #[test]
    fn a_switch_keeps_the_total_width() {
        // 22 and 28: a pill and the shown one, each with its gap; the second becomes the
        // shown one, then a third one is added and the first removed, together.
        let cases = [
            ([28.0, 22.0, 0.0], [22.0, 28.0, 0.0]),
            ([22.0, 28.0, 0.0], [0.0, 28.0, 22.0]),
        ];
        for (from, to) in cases {
            for frame in 0..=100 {
                let t = frame as f64 / 100.0;
                let now = from.iter().zip(to).map(|(f, t2)| f + (t2 - f) * t);
                let widths = whole_pixels(now);
                assert_eq!(widths.iter().sum::<i32>(), 50, "at {t}: {widths:?}");
            }
        }
    }

    #[test]
    fn removed_pills_stay_where_they_were() {
        assert_eq!(merge(&[1, 2, 3], &[1, 3]), [1, 2, 3]);
        assert_eq!(merge(&[1, 2, 3], &[2, 3]), [1, 2, 3]);
        assert_eq!(merge(&[1, 2], &[1, 2, 4]), [1, 2, 4]);
        assert_eq!(merge(&[1, 2, 3], &[4]), [1, 2, 3, 4]);
        assert_eq!(merge(&[], &[5, 6]), [5, 6]);
    }
}
