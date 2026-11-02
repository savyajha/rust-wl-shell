//! The Notification Centre, as macOS's: a solid panel down the right of an output, below
//! the bar, opened by its clock. Do Not Disturb, then the notifications' history, grouped
//! by app (the app with the newest first), newest first in each group, each with × and
//! swiped away as a banner is, and "Clear all". It closes on Escape, on the clock, and on
//! a click anywhere else: it takes the keyboard on demand, which niri moves away on any
//! click outside it.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk4::{self as gtk, gdk, glib, prelude::*};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use shell_core::config::Config;
use shell_core::notifications::{Notification, Notifications, Request};
use tokio::sync::mpsc::UnboundedSender;

use crate::notifications::{Banners, act, close, content, swipe};
use crate::popover::{self, Switch, title};

/// Its slide in and out: its whole surface moved, by its right margin.
const SLIDE: Duration = Duration::from_millis(200);
/// A step of the slide: about a frame.
const STEP: Duration = Duration::from_millis(8);
/// Its width less this is past the output's edge when it starts and ends its slide: one
/// pixel on the output, as niri gives frames only to a surface it shows.
const OFF: i32 = -1;

pub struct Centre {
    width: i32,
    gap: i32,
    swipe: f64,
    requests: UnboundedSender<Request>,
    /// Hidden while it is open.
    banners: Rc<RefCell<Banners>>,
    /// The last state, shown when it opens and while it is open.
    state: RefCell<Option<Notifications>>,
    /// Its window, only while it is open: one hidden would be GTK's last toplevel once no
    /// bar is left, which its AT-SPI root crashes on (`Banners`).
    open: RefCell<Option<Open>>,
}

struct Open {
    window: gtk::Window,
    /// Its slide: the latest one started moves it, an earlier one stops.
    slides: Rc<Cell<u32>>,
    /// The clock that opened it, told (for screen readers) that it is expanded.
    clock: gtk::Button,
    dnd: Switch,
    header: gtk::Box,
    list: gtk::Box,
    /// The history the list shows: built again only when it changes (not, say, for a
    /// banner's time), so that a card keeps its hover, a swipe and the keyboard.
    shown: RefCell<Option<Vec<Notification>>>,
}

impl Centre {
    pub fn new(
        config: &Config,
        requests: UnboundedSender<Request>,
        banners: Rc<RefCell<Banners>>,
    ) -> Self {
        Self {
            width: config.notification_centre_width as i32,
            gap: config.notification_gap as i32,
            swipe: f64::from(config.swipe_distance),
            requests,
            banners,
            state: RefCell::default(),
            open: RefCell::default(),
        }
    }

    pub fn is_open(&self) -> bool {
        self.open.borrow().is_some()
    }

    /// The notifications' state: shown now if it is open, else when it opens.
    pub fn show(self: &Rc<Self>, state: &Notifications) {
        self.state.replace(Some(state.clone()));
        self.fill();
    }

    /// Open it on `monitor`, from `clock` (its own there).
    pub fn open(self: &Rc<Self>, monitor: &gdk::Monitor, clock: &gtk::Button) {
        if self.is_open() {
            return;
        }
        popover::close_open();
        self.banners.borrow_mut().hide(true);
        let window = gtk::Window::builder()
            .css_classes(["centre"])
            .default_width(self.width)
            .build();
        window.init_layer_shell();
        window.set_namespace(Some("rust-wl-shell-centre"));
        window.set_layer(Layer::Top);
        window.set_keyboard_mode(KeyboardMode::OnDemand);
        window.set_monitor(Some(monitor));
        for edge in [Edge::Top, Edge::Right, Edge::Bottom] {
            window.set_anchor(edge, true);
            window.set_margin(edge, self.gap);
        }
        // Mapped past the edge, then slid in.
        let hidden = -(self.width + OFF);
        window.set_margin(Edge::Right, hidden);
        window.update_property(&[gtk::accessible::Property::Label("Notification Centre")]);
        let requests = self.requests.clone();
        let dnd = Switch::new("Do Not Disturb", move |on| {
            let _ = requests.send(Request::Dnd(on));
        });
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        header.add_css_class("header");
        let notifications = title("Notifications");
        notifications.set_hexpand(true);
        let clear = gtk::Button::builder()
            .label("Clear all")
            .css_classes(["clear"])
            .build();
        let requests = self.requests.clone();
        clear.connect_clicked(move |_| {
            let _ = requests.send(Request::ClearAll);
        });
        header.append(&notifications);
        header.append(&clear);
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        list.add_css_class("list");
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&list)
            .build();
        let panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
        panel.add_css_class("panel");
        panel.append(&dnd.row);
        panel.append(&header);
        panel.append(&scrolled);
        window.set_child(Some(&panel));
        let keys = gtk::EventControllerKey::new();
        let centre = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, _| {
            if key != gdk::Key::Escape {
                return glib::Propagation::Proceed;
            }
            if let Some(centre) = centre.upgrade() {
                centre.close();
            }
            glib::Propagation::Stop
        });
        window.add_controller(keys);
        // niri gives it the keyboard as it maps, and takes it on any click elsewhere. Only
        // the open window's: a closing one's unmap ends its own too.
        let (centre, had) = (Rc::downgrade(self), Cell::new(false));
        window.connect_is_active_notify(move |window| {
            if window.is_active() {
                had.set(true);
            } else if had.get()
                && let Some(centre) = centre.upgrade()
                && centre
                    .open
                    .borrow()
                    .as_ref()
                    .is_some_and(|o| &o.window == window)
            {
                centre.close();
            }
        });
        window.present();
        let slides = Rc::new(Cell::new(0));
        slide(&window, &slides, self.gap);
        clock.update_state(&[gtk::accessible::State::Expanded(Some(true))]);
        self.open.replace(Some(Open {
            window,
            slides,
            clock: clock.clone(),
            dnd,
            header,
            list,
            shown: RefCell::default(),
        }));
        self.fill();
    }

    pub fn close(&self) {
        let Some(open) = self.open.take() else {
            return;
        };
        self.banners.borrow_mut().hide(false);
        open.clock
            .update_state(&[gtk::accessible::State::Expanded(Some(false))]);
        let window = open.window;
        slide(&window, &open.slides, -(self.width + OFF));
        // Gone once it has slid out: on a timer, not the slide's end, which needs frames
        // an output gone or locked does not draw.
        glib::timeout_add_local_once(SLIDE, move || window.destroy());
    }

    /// Its contents, from the last state, while it is open.
    fn fill(self: &Rc<Self>) {
        let (open, state) = (self.open.borrow(), self.state.borrow());
        let (Some(open), Some(state)) = (open.as_ref(), state.as_ref()) else {
            return;
        };
        open.dnd.show(state.dnd);
        if open.shown.borrow().as_ref() == Some(&state.history) {
            return;
        }
        open.shown.replace(Some(state.history.clone()));
        open.header.set_visible(!state.history.is_empty());
        while let Some(child) = open.list.first_child() {
            open.list.remove(&child);
        }
        if state.history.is_empty() {
            let empty = gtk::Label::builder()
                .label("No notifications")
                .xalign(0.0)
                .build();
            empty.add_css_class("empty");
            open.list.append(&empty);
        }
        for (app, notifications) in groups(&state.history) {
            let group = gtk::Box::new(gtk::Orientation::Vertical, 0);
            group.add_css_class("group");
            group.append(&title(app));
            for n in notifications {
                group.append(&self.card(n));
            }
            open.list.append(&group);
        }
    }

    /// A notification's card: swiped away as a banner, a click on it its default action
    /// (when it still has one: not after a restart), which closes the centre.
    fn card(self: &Rc<Self>, n: &Notification) -> gtk::Overlay {
        // Its time ("5m") as of when the list was built (on opening and on each change).
        let card = content(n, None, true);
        card.add_overlay(&close(n, self.requests.clone()));
        let moved = follow_swipe(&card, self.swipe);
        let (requests, id) = (self.requests.clone(), n.id);
        swipe(&card, self.swipe, true, moved, move || {
            let _ = requests.send(Request::Dismiss(id));
        });
        if n.actions.iter().any(|(key, _)| key == "default") {
            let click = gtk::GestureClick::new();
            let (requests, centre) = (self.requests.clone(), Rc::downgrade(self));
            click.connect_released(move |click, _, _, _| {
                if let Some(widget) = click.widget() {
                    act(&requests, id, "default", &widget);
                }
                if let Some(centre) = centre.upgrade() {
                    centre.close();
                }
            });
            card.add_controller(click);
        }
        card
    }
}

/// Slide `window` (its whole surface, so niri's shadow with it, its contents keeping their
/// size: no reflow) from its right margin now to `to` over `SLIDE`, easing out, a step
/// every `STEP` (each margin goes out with the commit gtk4-layer-shell asks GTK for); a
/// slide it was in stops (`slides` counts them). On a timer rather than the frame clock,
/// which does not tick for a surface niri has no new frame for.
fn slide(window: &gtk::Window, slides: &Rc<Cell<u32>>, to: i32) {
    let (from, start) = (window.margin(Edge::Right), Instant::now());
    let slide = slides.get() + 1;
    slides.set(slide);
    let (window, slides) = (window.downgrade(), slides.clone());
    glib::timeout_add_local(STEP, move || {
        let Some(window) = window.upgrade().filter(|_| slides.get() == slide) else {
            return glib::ControlFlow::Break;
        };
        let t = (start.elapsed().as_secs_f64() / SLIDE.as_secs_f64()).min(1.0);
        let eased = 1.0 - (1.0 - t).powi(3);
        window.set_margin(
            Edge::Right,
            from + ((to - from) as f64 * eased).round() as i32,
        );
        match t < 1.0 {
            true => glib::ControlFlow::Continue,
            false => glib::ControlFlow::Break,
        }
    });
}

/// A card following a swipe: shifted by its margins, and fading.
fn follow_swipe(card: &gtk::Overlay, distance: f64) -> impl Fn(f64) + Clone + 'static {
    let card = card.downgrade();
    move |dx: f64| {
        if let Some(card) = card.upgrade() {
            card.set_margin_start(dx.max(0.0) as i32);
            card.set_margin_end((-dx).max(0.0) as i32);
            card.set_opacity(1.0 - (dx.abs() / (2.0 * distance)).min(0.8));
        }
    }
}

/// The history by app: the app with the newest notification first, each app's newest
/// first.
fn groups(history: &[Notification]) -> Vec<(&str, Vec<&Notification>)> {
    let mut groups: Vec<(&str, Vec<&Notification>)> = Vec::new();
    for n in history {
        match groups.iter_mut().find(|(app, _)| *app == n.app) {
            Some((_, list)) => list.push(n),
            None => groups.push((&n.app, vec![n])),
        }
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouped_by_app_newest_first() {
        let n = |id, app: &str| Notification {
            id,
            app: app.into(),
            ..Default::default()
        };
        let history = [
            n(5, "Mail"),
            n(4, "Chat"),
            n(3, "Mail"),
            n(2, "Chat"),
            n(1, "Clock"),
        ];
        let grouped: Vec<(&str, Vec<u32>)> = groups(&history)
            .into_iter()
            .map(|(app, list)| (app, list.iter().map(|n| n.id).collect()))
            .collect();
        assert_eq!(
            grouped,
            [
                ("Mail", vec![5, 3]),
                ("Chat", vec![4, 2]),
                ("Clock", vec![1])
            ]
        );
    }
}
