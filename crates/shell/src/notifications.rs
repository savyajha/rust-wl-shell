//! Notifications on screen: the banners, each an opaque panel of its own on the overlay
//! layer of the focused output, at the configured corner, newest nearest it; and what
//! banners and the clock popover's history share: a notification's content, its ×, and the
//! swipe that dismisses one.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk4::{self as gtk, cairo, gdk, gio, glib, pango, prelude::*};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use shell_core::config::{Config, Corner};
use shell_core::notifications::{Notification, Notifications, Request};
use tokio::sync::mpsc::UnboundedSender;

/// Between a banner and its menu ("Options").
const MENU_GAP: i32 = 6;
/// A banner's first moments: the pointer entering it then was there when it showed.
const SETTLED: Duration = Duration::from_millis(500);
/// The ×'s size (the stylesheet's `--close-size`, set from here): a banner's window has
/// half of it around its card's top and right, for its × centred on the card's corner.
pub const CLOSE_SIZE: i32 = 20;

/// A banner's pending timeout, if it has one.
type Timer = Rc<RefCell<Option<glib::SourceId>>>;

/// A banner's countdown to `Expired`, which runs whether its window is shown or not.
#[derive(Clone)]
struct Countdown {
    timer: Timer,
    /// The time left, and since when it runs (`None`: paused by the pointer).
    left: Rc<Cell<(Duration, Option<Instant>)>>,
    requests: UnboundedSender<Request>,
    id: u32,
}

pub struct Banners {
    corner: Corner,
    /// From the output's edges and between banners; their width; a swipe's distance.
    gap: i32,
    width: i32,
    swipe: f64,
    requests: UnboundedSender<Request>,
    /// Newest first, as shown, each with its window (none while hidden) and countdown
    /// (none for one that stays).
    shown: Vec<(Notification, Option<gtk::Window>, Option<Countdown>)>,
    /// While the clock's popover (the history) is open: the banners hidden, their time
    /// still running. Their windows are destroyed rather than hidden: GTK 4.22's AT-SPI
    /// root answers an index past its shown windows with its last window, whose
    /// accessible a hidden window has unrealized, and crashes on its missing path.
    hidden: bool,
}

impl Banners {
    pub fn new(config: &Config, requests: UnboundedSender<Request>) -> Self {
        Self {
            corner: config.notification_position,
            gap: config.notification_gap as i32,
            width: config.notification_width as i32,
            swipe: f64::from(config.swipe_distance),
            requests,
            shown: Vec::new(),
            hidden: false,
        }
    }

    /// Hide the banners (`true`), or show them again.
    pub fn hide(&mut self, hidden: bool) {
        self.hidden = hidden;
        for i in 0..self.shown.len() {
            let (n, window, countdown) = &self.shown[i];
            match (hidden, window) {
                (true, Some(window)) => {
                    // Its countdown runs on: kept from the window's own end.
                    let source = countdown.as_ref().and_then(|c| c.timer.take());
                    window.destroy();
                    match (countdown, source) {
                        (Some(c), Some(source)) => *c.timer.borrow_mut() = Some(source),
                        // Paused by the pointer, which has left with the window.
                        (Some(c), None) if c.left.get().1.is_none() => c.resume(),
                        _ => {}
                    }
                    self.shown[i].1 = None;
                }
                (false, None) => {
                    let (n, countdown) = (n.clone(), countdown.clone());
                    // No output: niri's choice, the focused one now (that of their last
                    // showing may be gone).
                    self.shown[i].1 = Some(self.banner(&n, None, countdown.as_ref()));
                }
                _ => {}
            }
        }
        self.stack();
    }

    /// Show `state`'s banners on `monitor`: new ones built (a replaced one again), gone
    /// ones closed, all stacked.
    pub fn show(&mut self, state: &Notifications, monitor: Option<&gdk::Monitor>) {
        self.shown.retain(|(shown, window, countdown)| {
            let kept = state.banners.contains(shown);
            if !kept {
                // Gone from the store already: its timer is not to tell it.
                if let Some(source) = countdown.as_ref().and_then(|c| c.timer.take()) {
                    source.remove();
                }
                if let Some(window) = window {
                    window.destroy();
                }
            }
            kept
        });
        for n in &state.banners {
            if !self.shown.iter().any(|(shown, _, _)| shown == n) {
                let countdown = n.timeout.map(|t| Countdown::start(t, &self.requests, n.id));
                let window = (!self.hidden).then(|| self.banner(n, monitor, countdown.as_ref()));
                self.shown.push((n.clone(), window, countdown));
            }
        }
        // Newest nearest the corner: in the order of the state's banners.
        self.shown
            .sort_by_key(|(shown, _, _)| state.banners.iter().position(|n| n == shown));
        self.stack();
    }

    /// The banners shown stacked from the corner, their cards a gap apart (and from the
    /// edge: less the room for the × above each card, as far as the edge lets it).
    fn stack(&self) {
        let (vertical, mut offset) = match self.corner {
            Corner::TopLeft | Corner::Top | Corner::TopRight => {
                (Edge::Top, (self.gap - CLOSE_SIZE / 2).max(0))
            }
            _ => (Edge::Bottom, self.gap),
        };
        let width = self.width + CLOSE_SIZE / 2;
        for window in self
            .shown
            .iter()
            .filter_map(|(_, window, _)| window.as_ref())
        {
            window.set_margin(vertical, offset);
            let (_, height, _, _) = window.measure(gtk::Orientation::Vertical, width);
            offset += height + self.gap - CLOSE_SIZE / 2;
        }
    }

    fn banner(
        &self,
        n: &Notification,
        monitor: Option<&gdk::Monitor>,
        countdown: Option<&Countdown>,
    ) -> gtk::Window {
        let window = gtk::Window::builder()
            .css_classes(["notification"])
            .default_width(self.width + CLOSE_SIZE / 2)
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
        // From the edge: the card a gap away, the × beside it as far as the edge lets it.
        let margin = match horizontal {
            Some(Edge::Right) => (self.gap - CLOSE_SIZE / 2).max(0),
            _ => self.gap,
        };
        if let Some(edge) = horizontal {
            window.set_anchor(edge, true);
            window.set_margin(edge, margin);
        }
        let requests = self.requests.clone();
        let id = n.id;
        // One action a button on the right; more, a menu ("Options"), as macOS's.
        let actions: Vec<_> = n
            .actions
            .iter()
            .filter(|(key, _)| key != "default")
            .collect();
        let (side, menu) = match actions[..] {
            [] => (None, None),
            [(key, label)] => {
                let button = gtk::Button::with_label(label);
                let (requests, key) = (requests.clone(), key.clone());
                button.connect_clicked(move |button| act(&requests, id, &key, button));
                (Some(button.upcast::<gtk::Widget>()), None)
            }
            _ => {
                let (rows, menu) = (
                    gtk::Box::new(gtk::Orientation::Vertical, 0),
                    gtk::Popover::new(),
                );
                for (key, label) in actions {
                    let text = gtk::Label::builder().label(label).xalign(0.0).build();
                    let row = gtk::Button::builder()
                        .child(&text)
                        .css_classes(["row"])
                        .build();
                    row.update_property(&[gtk::accessible::Property::Label(label)]);
                    let (requests, key, menu) = (requests.clone(), key.clone(), menu.clone());
                    row.connect_clicked(move |row| {
                        act(&requests, id, &key, row);
                        menu.popdown();
                    });
                    rows.append(&row);
                }
                menu.set_child(Some(&rows));
                menu.add_css_class("item-popover");
                menu.set_has_arrow(false);
                let button = gtk::MenuButton::builder()
                    .label("Options")
                    .popover(&menu)
                    .build();
                button.set_always_show_arrow(true);
                (Some(button.upcast()), Some(menu))
            }
        };
        if let Some(side) = &side {
            side.add_css_class("action");
        }
        let card = content(n, side.as_ref(), None);
        if let (Some(menu), Some(button)) = (&menu, &side) {
            // The menu takes the keyboard while open (Escape), as the bar's popovers; below
            // the banner, at the button's right edge, as macOS's.
            menu.set_halign(gtk::Align::End);
            let (shown, button) = (window.clone(), button.clone());
            menu.connect_show(move |menu| {
                if let Some(b) = button.compute_bounds(&shown) {
                    let below = shown.height() as f32 - b.y() - b.height();
                    menu.set_offset(0, below as i32 + MENU_GAP);
                }
                shown.set_keyboard_mode(KeyboardMode::OnDemand);
            });
            let closed = window.clone();
            menu.connect_closed(move |_| closed.set_keyboard_mode(KeyboardMode::None));
        }
        // The × centred on the card's top-right corner, over the window's margin there.
        let frame = gtk::Overlay::builder()
            .child(&card)
            .css_classes(["banner"])
            .build();
        let close = close(n, requests.clone());
        close.set_halign(gtk::Align::End);
        frame.add_overlay(&close);
        card.set_margin_top(CLOSE_SIZE / 2);
        card.set_margin_end(CLOSE_SIZE / 2);
        window.set_child(Some(&frame));
        input_region(&window, &card, &close);
        // GTK closes an open popover on a click outside it, and keeps the click: closed
        // first, by the pointer coming onto the banner, the click (on ×) reaches it. While
        // the popover holds its grab, the banner gets no motion, only the pointer's enter:
        // one at once is the banner showing under a still pointer, which leaves it open.
        let hover = gtk::EventControllerMotion::new();
        let built = Instant::now();
        hover.connect_enter(move |_, _, _| {
            if built.elapsed() > SETTLED {
                crate::popover::close_open();
            }
        });
        frame.add_controller(hover);
        // The banner follows a swipe sideways, as far as its anchored edge lets it.
        let swiped = self.swipe;
        let moved = glib::clone!(
            #[weak]
            window,
            move |dx: f64| {
                window.set_opacity(1.0 - (dx.abs() / (2.0 * swiped)).min(0.8));
                match horizontal {
                    Some(Edge::Right) => window.set_margin(Edge::Right, margin - dx as i32),
                    Some(edge) => window.set_margin(edge, margin + dx as i32),
                    None => {}
                }
            }
        );
        let dismissed = requests.clone();
        swipe(&card, self.swipe, horizontal.is_some(), moved, move || {
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
        if let Some(countdown) = countdown {
            countdown.follow(&frame, &window, menu.as_ref());
        }
        window.set_visible(true);
        window
    }
}

impl Countdown {
    /// Send `Expired(id)` after `timeout`.
    fn start(timeout: Duration, requests: &UnboundedSender<Request>, id: u32) -> Self {
        let countdown = Self {
            timer: Timer::default(),
            left: Rc::new(Cell::new((timeout, None))),
            requests: requests.clone(),
            id,
        };
        countdown.resume();
        countdown
    }

    fn resume(&self) {
        let (requests, fired, id) = (self.requests.clone(), self.timer.clone(), self.id);
        let source = glib::timeout_add_local_once(self.left.get().0, move || {
            // Gone once fired: not to be removed again.
            fired.take();
            let _ = requests.send(Request::Expired(id));
        });
        self.left.set((self.left.get().0, Some(Instant::now())));
        *self.timer.borrow_mut() = Some(source);
    }

    /// The clock stopped while the pointer is over `card` (or its ×), or its `menu` is open. Destroyed
    /// with `window` still pending (not by `Banners`, which takes it first: its output
    /// gone), the banner is expired at once.
    fn follow(&self, card: &gtk::Overlay, window: &gtk::Window, menu: Option<&gtk::Popover>) {
        let hover = gtk::EventControllerMotion::new();
        let hovered = Rc::new(Cell::new(false));
        let (paused, over) = (self.clone(), hovered.clone());
        hover.connect_enter(move |_, _, _| {
            over.set(true);
            if let (Some(source), (time, Some(since))) = (paused.timer.take(), paused.left.get()) {
                source.remove();
                paused
                    .left
                    .set((time.saturating_sub(since.elapsed()), None));
            }
        });
        // Paused by the hover, not fired; not while the pointer is in the menu.
        let paused = |c: &Countdown| c.timer.borrow().is_none() && c.left.get().1.is_none();
        let (resumed, open, over) = (self.clone(), menu.cloned(), hovered.clone());
        hover.connect_leave(move |_| {
            over.set(false);
            if paused(&resumed) && !open.as_ref().is_some_and(|m| m.is_visible()) {
                resumed.resume();
            }
        });
        card.add_controller(hover);
        let resumed = self.clone();
        // Closed with the pointer back over the card: still paused.
        menu.inspect(|m| {
            m.connect_closed(move |_| {
                if paused(&resumed) && !hovered.get() {
                    resumed.resume();
                }
            });
        });
        let pending = self.clone();
        window.connect_destroy(move |_| {
            if let Some(source) = pending.timer.take() {
                source.remove();
                let _ = pending.requests.send(Request::Expired(pending.id));
            }
        });
    }
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

/// A notification's card, as macOS's: its app's icon (full colour, from its desktop file,
/// else the icon it sent), or its image with the app's icon as a badge on it; the summary,
/// with the time after it in the history (`times`, to refresh), the body (three lines at
/// most); `side` (its actions) on the right. An overlay, for its × (`close`).
pub fn content(
    n: &Notification,
    side: Option<&gtk::Widget>,
    times: Option<&Times>,
) -> gtk::Overlay {
    let main = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    main.add_css_class("main");
    let slot = gtk::Overlay::new();
    slot.add_css_class("slot");
    slot.set_valign(gtk::Align::Start);
    let icon = app_icon(n);
    match &n.image {
        Some(image) => {
            let format = match image.alpha {
                true => gdk::MemoryFormat::R8g8b8a8,
                false => gdk::MemoryFormat::R8g8b8,
            };
            let bytes = glib::Bytes::from(&image.pixels[..]);
            let stride = image.rowstride as usize;
            let texture =
                gdk::MemoryTexture::new(image.width, image.height, format, &bytes, stride);
            let picture = gtk::Image::from_paintable(Some(&texture));
            picture.add_css_class("picture");
            slot.set_child(Some(&picture));
            icon.add_css_class("badge");
            icon.set_halign(gtk::Align::End);
            icon.set_valign(gtk::Align::End);
            slot.add_overlay(&icon);
        }
        None => {
            icon.add_css_class("app-icon");
            slot.set_child(Some(&icon));
        }
    }
    main.append(&slot);
    let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
    text.add_css_class("text");
    text.set_hexpand(true);
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let summary = gtk::Label::builder()
        .label(&n.summary)
        .xalign(0.0)
        .hexpand(true)
        .wrap(true)
        .wrap_mode(pango::WrapMode::WordChar)
        .lines(2)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    summary.add_css_class("summary");
    top.append(&summary);
    if let Some(times) = times {
        let time = gtk::Label::new(Some(&ago(n.time)));
        time.add_css_class("time");
        top.append(&time);
        times.borrow_mut().push((time, n.time));
    }
    text.append(&top);
    if !n.body.is_empty() {
        let body = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .lines(3)
            .ellipsize(pango::EllipsizeMode::End)
            .build();
        // The spec's markup, else (for one that does not parse) the text as it is.
        match pango::parse_markup(&n.body, '\0') {
            Ok(_) => body.set_markup(&n.body),
            Err(_) => body.set_text(&n.body),
        }
        text.append(&body);
    }
    main.append(&text);
    if let Some(side) = side {
        side.set_valign(gtk::Align::Center);
        main.append(side);
    }
    gtk::Overlay::builder()
        .child(&main)
        .css_classes(["card"])
        .build()
}

/// A notification's ×, which dismisses it, shown while the pointer is over its card (at the
/// top left, unless moved); named after the summary for screen readers.
pub fn close(n: &Notification, requests: UnboundedSender<Request>) -> gtk::Button {
    let close = gtk::Button::builder()
        .icon_name("window-close-symbolic")
        .css_classes(["close"])
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Start)
        .build();
    let id = n.id;
    let name = format!("Dismiss \"{}\"", n.summary);
    close.update_property(&[gtk::accessible::Property::Label(&name)]);
    close.connect_clicked(move |_| {
        let _ = requests.send(Request::Dismiss(id));
    });
    close
}

/// A banner's window takes the pointer only on its `card` and its `close`, not on the
/// margin around them (the shell's own windows have none of GTK's shapes to keep).
fn input_region(window: &gtk::Window, card: &gtk::Overlay, close: &gtk::Button) {
    let (card, close) = (card.clone(), close.clone());
    window.connect_realize(move |window| {
        let (window, card, close) = (window.clone(), card.clone(), close.clone());
        let Some(surface) = window.surface() else {
            return;
        };
        // After GTK's own handler: the widgets laid out for the size.
        surface.connect_layout(move |surface, _, _| {
            let rectangles: Vec<_> = [card.upcast_ref::<gtk::Widget>(), close.upcast_ref()]
                .iter()
                .filter_map(|w| w.compute_bounds(&window))
                .map(|b| {
                    let (x, y) = (b.x() as i32, b.y() as i32);
                    cairo::RectangleInt::new(x, y, b.width() as i32, b.height() as i32)
                })
                .collect();
            surface.set_input_region(Some(&cairo::Region::create_rectangles(&rectangles)));
        });
    });
}

/// The app's icon: its desktop file's (`desktop-entry`), else the one it sent (a name, a
/// path or a `file://` URI), else a generic one.
fn app_icon(n: &Notification) -> gtk::Image {
    let desktop = n.desktop_entry.as_ref();
    let info = desktop.and_then(|e| gio_unix::DesktopAppInfo::new(&format!("{e}.desktop")));
    if let Some(icon) = info.and_then(|i| i.icon()) {
        return gtk::Image::from_gicon(&icon);
    }
    let path = match n.icon.starts_with("file://") {
        // A URI's path decoded ("%20").
        true => gio::File::for_uri(&n.icon).path(),
        false => n.icon.starts_with('/').then(|| n.icon.clone().into()),
    };
    match path {
        Some(path) => gtk::Image::from_file(path),
        None if n.icon.is_empty() => gtk::Image::from_icon_name("application-x-executable"),
        None => gtk::Image::from_icon_name(&n.icon),
    }
}

/// The history's times ("5m"), with when each notification came, to refresh.
pub type Times = RefCell<Vec<(gtk::Label, u64)>>;

/// How long ago `time` (seconds since 1970) was: "now", "5m", "2h", "3d".
pub fn ago(time: u64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    let secs = now.map_or(0, |d| d.as_secs()).saturating_sub(time);
    match secs {
        0..60 => "now".into(),
        60..3600 => format!("{}m", secs / 60),
        3600..86_400 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

/// A horizontal swipe on `widget`, by a drag or by a two-finger touchpad scroll: `moved`
/// with the distance so far as it goes (0 again when it stops short), and `swiped` once
/// it passes `distance`. `follows`: `moved` moves `widget` itself.
pub fn swipe(
    widget: &impl IsA<gtk::Widget>,
    distance: f64,
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
        if at.replace(0.0).abs() > distance {
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
        if at.get().abs() > distance {
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
