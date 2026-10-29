//! Notifications on screen: the banners, each a frosted panel of its own on the overlay
//! layer of the focused output, at the configured corner, newest nearest it; and what
//! banners and the clock popover's history share: a notification's content, and the swipe
//! that dismisses one.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk4::{self as gtk, gdk, gio, glib, pango, prelude::*};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use shell_core::config::Corner;
use shell_core::notifications::{Notification, Notifications, Request};
use tokio::sync::mpsc::UnboundedSender;

use crate::blur::Blur;

/// How far a swipe goes before it dismisses.
const SWIPE: f64 = 100.0;
/// From the output's edges, and between banners.
const GAP: i32 = 8;
const WIDTH: i32 = 360;

/// A banner's pending timeout, if it has one.
type Timer = Rc<RefCell<Option<glib::SourceId>>>;

pub struct Banners {
    corner: Corner,
    requests: UnboundedSender<Request>,
    blur: Option<Rc<Blur>>,
    /// Newest first, as shown, each with its timer.
    shown: Vec<(Notification, gtk::Window, Timer)>,
    /// While the clock's popover (the history) is open: the banners hidden, their time
    /// still running.
    hidden: bool,
}

impl Banners {
    pub fn new(corner: Corner, requests: UnboundedSender<Request>, blur: Option<Rc<Blur>>) -> Self {
        Self {
            corner,
            requests,
            blur,
            shown: Vec::new(),
            hidden: false,
        }
    }

    /// Hide the banners (`true`), or show them again.
    pub fn hide(&mut self, hidden: bool) {
        self.hidden = hidden;
        for (_, window, _) in &self.shown {
            window.set_visible(!hidden);
        }
    }

    /// Show `state`'s banners on `monitor`: new ones built (a replaced one again), gone
    /// ones closed, all stacked.
    pub fn show(&mut self, state: &Notifications, monitor: Option<&gdk::Monitor>) {
        self.shown.retain(|(shown, window, timer)| {
            let kept = state.banners.contains(shown);
            if !kept {
                // Gone from the store already: its timer is not to tell it.
                if let Some(source) = timer.take() {
                    source.remove();
                }
                window.destroy();
            }
            kept
        });
        for n in &state.banners {
            if !self.shown.iter().any(|(shown, _, _)| shown == n) {
                let (window, timer) = self.banner(n, monitor);
                self.shown.push((n.clone(), window, timer));
            }
        }
        // Newest nearest the corner: in the order of the state's banners.
        self.shown
            .sort_by_key(|(shown, _, _)| state.banners.iter().position(|n| n == shown));
        let vertical = match self.corner {
            Corner::TopLeft | Corner::Top | Corner::TopRight => Edge::Top,
            _ => Edge::Bottom,
        };
        let mut offset = GAP;
        for (_, window, _) in &self.shown {
            window.set_margin(vertical, offset);
            let (_, height, _, _) = window.measure(gtk::Orientation::Vertical, WIDTH);
            offset += height + GAP;
        }
    }

    fn banner(&self, n: &Notification, monitor: Option<&gdk::Monitor>) -> (gtk::Window, Timer) {
        let window = gtk::Window::builder()
            .css_classes(["notification"])
            .default_width(WIDTH)
            .build();
        window.init_layer_shell();
        window.set_namespace(Some("rust-wl-shell-notification"));
        window.set_layer(Layer::Overlay);
        window.set_keyboard_mode(KeyboardMode::None);
        window.set_monitor(monitor);
        let (vertical, horizontal) = match self.corner {
            Corner::TopLeft => (Edge::Top, Some(Edge::Left)),
            Corner::Top => (Edge::Top, None),
            Corner::TopRight => (Edge::Top, Some(Edge::Right)),
            Corner::BottomLeft => (Edge::Bottom, Some(Edge::Left)),
            Corner::Bottom => (Edge::Bottom, None),
            Corner::BottomRight => (Edge::Bottom, Some(Edge::Right)),
        };
        window.set_anchor(vertical, true);
        if let Some(edge) = horizontal {
            window.set_anchor(edge, true);
            window.set_margin(edge, GAP);
        }
        let requests = self.requests.clone();
        let id = n.id;
        let card = content(n, requests.clone());
        let buttons = n.actions.iter().filter(|(key, _)| key != "default").take(3);
        let row = gtk::Box::builder()
            .homogeneous(true)
            .spacing(6)
            .css_classes(["actions"])
            .build();
        for (key, label) in buttons {
            let button = gtk::Button::with_label(label);
            let (requests, key) = (requests.clone(), key.clone());
            button.connect_clicked(move |button| act(&requests, id, &key, button));
            row.append(&button);
        }
        if row.first_child().is_some() {
            card.append(&row);
        }
        window.set_child(Some(&card));
        // The banner follows a swipe sideways, as far as its anchored edge lets it.
        let moved = glib::clone!(
            #[weak]
            window,
            move |dx: f64| {
                window.set_opacity(1.0 - (dx.abs() / (2.0 * SWIPE)).min(0.8));
                match horizontal {
                    Some(Edge::Right) => window.set_margin(Edge::Right, GAP - dx as i32),
                    Some(edge) => window.set_margin(edge, GAP + dx as i32),
                    None => {}
                }
            }
        );
        let dismissed = requests.clone();
        swipe(&card, horizontal.is_some(), moved, move || {
            let _ = dismissed.send(Request::Dismiss(id));
        });
        if n.actions.iter().any(|(key, _)| key == "default") {
            let click = gtk::GestureClick::new();
            let requests = requests.clone();
            click.connect_released(move |click, _, _, _| {
                if let Some(widget) = click.widget() {
                    act(&requests, id, "default", &widget);
                }
            });
            card.add_controller(click);
        }
        let timer = match n.timeout {
            Some(timeout) => expire(&card, &window, timeout, requests, id),
            None => Timer::default(),
        };
        Blur::attach(&self.blur, &window, 14);
        window.set_visible(!self.hidden);
        (window, timer)
    }
}

/// Send `Expired(id)` after `timeout`, the clock stopped while the pointer is over `card`.
/// The timer goes with `window`; destroyed with it still pending (not by `Banners`,
/// which takes it first: its output gone), the banner is expired at once.
fn expire(
    card: &gtk::Box,
    window: &gtk::Window,
    timeout: Duration,
    requests: UnboundedSender<Request>,
    id: u32,
) -> Timer {
    // The time left, and since when it runs (`None`: paused).
    let left = Rc::new(Cell::new((timeout, Some(Instant::now()))));
    let timer = Timer::default();
    let gone = requests.clone();
    let start = {
        let (left, timer) = (left.clone(), timer.clone());
        move || {
            let (requests, fired) = (requests.clone(), timer.clone());
            let source = glib::timeout_add_local_once(left.get().0, move || {
                // Gone once fired: not to be removed again.
                fired.take();
                let _ = requests.send(Request::Expired(id));
            });
            left.set((left.get().0, Some(Instant::now())));
            *timer.borrow_mut() = Some(source);
        }
    };
    start();
    let hover = gtk::EventControllerMotion::new();
    let (paused, timer2) = (left.clone(), timer.clone());
    hover.connect_enter(move |_, _, _| {
        if let (Some(source), (time, Some(since))) = (timer2.take(), paused.get()) {
            source.remove();
            paused.set((time.saturating_sub(since.elapsed()), None));
        }
    });
    let resumed = timer.clone();
    hover.connect_leave(move |_| {
        // Paused by the hover, not fired.
        if resumed.borrow().is_none() && left.get().1.is_none() {
            start();
        }
    });
    card.add_controller(hover);
    let pending = timer.clone();
    window.connect_destroy(move |_| {
        if let Some(source) = pending.take() {
            source.remove();
            let _ = gone.send(Request::Expired(id));
        }
    });
    timer
}

/// Run action `key` of notification `id`, with an activation token from `widget`'s
/// display, so that the app may raise its window.
pub fn act(
    requests: &UnboundedSender<Request>,
    id: u32,
    key: &str,
    widget: &impl IsA<gtk::Widget>,
) {
    let context = widget.display().app_launch_context();
    let token = context
        .startup_notify_id(None::<&gio::AppInfo>, &[])
        .map(String::from);
    let _ = requests.send(Request::Action {
        id,
        key: key.into(),
        token,
    });
}

/// A notification's card: its app's icon and name with × after them, then its image (if
/// sent), summary and body.
pub fn content(n: &Notification, requests: UnboundedSender<Request>) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 4);
    card.add_css_class("card");
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    if !n.icon.is_empty() {
        // A URI's path decoded ("%20"), else a path or an icon's name.
        let path = match n.icon.starts_with("file://") {
            true => gio::File::for_uri(&n.icon).path(),
            false => n.icon.starts_with('/').then(|| n.icon.clone().into()),
        };
        let icon = match path {
            Some(path) => gtk::Image::from_file(path),
            None => gtk::Image::from_icon_name(&n.icon),
        };
        header.append(&icon);
    }
    let app = gtk::Label::builder()
        .label(&n.app)
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    app.add_css_class("app");
    header.append(&app);
    let close = gtk::Button::builder()
        .icon_name("window-close-symbolic")
        .css_classes(["close"])
        .build();
    let id = n.id;
    close.update_property(&[gtk::accessible::Property::Label("Dismiss")]);
    close.connect_clicked(move |_| {
        let _ = requests.send(Request::Dismiss(id));
    });
    header.append(&close);
    card.append(&header);
    let main = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    if let Some(image) = &n.image {
        let format = match image.alpha {
            true => gdk::MemoryFormat::R8g8b8a8,
            false => gdk::MemoryFormat::R8g8b8,
        };
        let bytes = glib::Bytes::from(&image.pixels[..]);
        let texture = gdk::MemoryTexture::new(
            image.width,
            image.height,
            format,
            &bytes,
            image.rowstride as usize,
        );
        let picture = gtk::Image::from_paintable(Some(&texture));
        picture.set_pixel_size(40);
        main.append(&picture);
    }
    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let summary = gtk::Label::builder()
        .label(&n.summary)
        .xalign(0.0)
        .wrap(true)
        .build();
    summary.add_css_class("summary");
    text.append(&summary);
    if !n.body.is_empty() {
        let body = gtk::Label::builder().xalign(0.0).wrap(true).build();
        // The spec's markup, else (for one that does not parse) the text as it is.
        match pango::parse_markup(&n.body, '\0') {
            Ok(_) => body.set_markup(&n.body),
            Err(_) => body.set_text(&n.body),
        }
        text.append(&body);
    }
    main.append(&text);
    card.append(&main);
    card
}

/// A horizontal swipe on `widget`, by a drag or by a two-finger touchpad scroll: `moved`
/// with the distance so far as it goes (0 again when it stops short), and `swiped` once
/// it passes `SWIPE`. `follows`: `moved` moves `widget` itself.
pub fn swipe(
    widget: &impl IsA<gtk::Widget>,
    follows: bool,
    moved: impl Fn(f64) + Clone + 'static,
    swiped: impl Fn() + Clone + 'static,
) {
    // The distance shown. A drag's offsets are in `widget`'s coordinates: when it moves
    // with them, each offset is from where it was last moved to.
    let shown = Rc::new(Cell::new(0.0));
    let drag = gtk::GestureDrag::new();
    let (follow, at) = (moved.clone(), shown.clone());
    drag.connect_drag_update(move |_, dx, _| {
        at.set(if follows { at.get() + dx } else { dx });
        follow(at.get());
    });
    let (end, at, done) = (moved.clone(), shown.clone(), swiped.clone());
    drag.connect_drag_end(move |_, _, _| {
        if at.replace(0.0).abs() > SWIPE {
            done();
        } else {
            end(0.0);
        }
    });
    widget.add_controller(drag);
    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::HORIZONTAL);
    let (follow, at) = (moved.clone(), shown.clone());
    scroll.connect_scroll(move |scroll, dx, _| {
        let step = if scroll.unit() == gdk::ScrollUnit::Wheel {
            40.0
        } else {
            1.0
        };
        at.set(at.get() - dx * step);
        if at.get().abs() > SWIPE {
            at.set(0.0);
            swiped();
        } else {
            follow(at.get());
        }
        glib::Propagation::Stop
    });
    scroll.connect_scroll_end(move |_| {
        if shown.replace(0.0) != 0.0 {
            moved(0.0);
        }
    });
    widget.add_controller(scroll);
}
