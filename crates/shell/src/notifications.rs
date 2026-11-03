//! Notifications on screen: the banners, each an opaque panel of its own on the overlay
//! layer of the focused output, at the configured corner, newest nearest it; and what
//! banners and the clock popover's history share: a notification's content, its ×, and the
//! swipe that dismisses one.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk4::{self as gtk, gdk, gio, glib, pango, prelude::*};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use shell_core::config::{Config, Corner};
use shell_core::notifications::{Notification, Notifications, Request};
use tokio::sync::mpsc::UnboundedSender;

/// Between the "Options" button and its menu.
const MENU_GAP: i32 = 6;
/// A banner's first moments, from its map: the pointer entering it then was there when it
/// showed.
const SETTLED: Duration = Duration::from_millis(500);

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
                    // Its countdown runs on; paused by the pointer, which has left with
                    // the window, it resumes.
                    if let Some(c) = countdown
                        && c.timer.borrow().is_none()
                        && c.left.get().1.is_none()
                    {
                        c.resume();
                    }
                    window.destroy();
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
                // Gone from the store already: its timer is not to tell it, nor to start
                // again (its menu closing with the window, as if paused by it).
                if let Some(c) = countdown {
                    if let Some(source) = c.timer.take() {
                        source.remove();
                    }
                    c.left.set((Duration::ZERO, Some(Instant::now())));
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

    /// The banners shown stacked from the corner, a gap apart.
    fn stack(&self) {
        let vertical = match self.corner {
            Corner::TopLeft | Corner::Top | Corner::TopRight => Edge::Top,
            _ => Edge::Bottom,
        };
        let mut offset = self.gap;
        for window in self
            .shown
            .iter()
            .filter_map(|(_, window, _)| window.as_ref())
        {
            window.set_margin(vertical, offset);
            let (_, height, _, _) = window.measure(gtk::Orientation::Vertical, self.width);
            offset += height + self.gap;
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
            .default_width(self.width)
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
            window.set_margin(edge, self.gap);
        }
        let requests = self.requests.clone();
        let id = n.id;
        // Two buttons at most, on the right: one or two actions each a button; more, the
        // first one's, then "Options", a menu of the others (`default` is a click on the
        // card).
        let actions: Vec<_> = n
            .actions
            .iter()
            .filter(|(key, _)| key != "default" && !chromium_settings(n, key))
            .collect();
        // A button for an action, labelled with its key if it has no label.
        let button = |label: &str, key: &str| {
            let button = gtk::Button::with_label(if label.is_empty() { key } else { label });
            let (requests, key) = (requests.clone(), key.to_owned());
            button.connect_clicked(move |button| act(&requests, id, &key, button));
            button
        };
        let (side, options) = match actions.split_first() {
            None => (None, None),
            Some(((key, label), rest)) => {
                let side = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                side.add_css_class("actions");
                // Short on the card: a long label ellipsized, the text and width kept; a
                // short one whole (else the body, wrapping, would take its width), up to
                // 10 characters, so that two still fit the card's width.
                let short = |button: gtk::Button| {
                    if let Some(text) = button.child().and_downcast::<gtk::Label>() {
                        let chars = text.label().chars().count().min(10) as i32;
                        text.set_ellipsize(pango::EllipsizeMode::End);
                        text.set_width_chars(chars);
                        text.set_max_width_chars(12);
                    }
                    button
                };
                side.append(&short(button(label, key)));
                if let [(key, label)] = rest {
                    side.append(&short(button(label, key)));
                }
                let options = (rest.len() > 1).then(|| {
                    let rows = gtk::Box::new(gtk::Orientation::Vertical, 0);
                    let menu = gtk::Popover::builder()
                        .child(&rows)
                        .css_classes(["item-popover", "menu"])
                        .has_arrow(false)
                        .build();
                    for (key, label) in rest {
                        let row = button(label, key);
                        row.set_css_classes(&["row"]);
                        if let Some(text) = row.child().and_downcast::<gtk::Label>() {
                            text.set_xalign(0.0);
                        }
                        let menu = menu.clone();
                        row.connect_clicked(move |_| menu.popdown());
                        rows.append(&row);
                    }
                    let options = gtk::MenuButton::builder()
                        .label("Options")
                        .popover(&menu)
                        .always_show_arrow(true)
                        .build();
                    side.append(&options);
                    (options, menu)
                });
                // Taking no focus, as the ×: a banner's window never has the keyboard (but
                // its menu, whose rows do), and GTK would give them its first focus.
                let mut child = side.first_child();
                while let Some(widget) = child {
                    widget.add_css_class("action");
                    // A menu button's own button takes its focus.
                    let focus = match widget.downcast_ref::<gtk::MenuButton>() {
                        Some(options) => options.first_child(),
                        None => Some(widget.clone()),
                    };
                    if let Some(focus) = focus {
                        focus.set_focusable(false);
                    }
                    child = widget.next_sibling();
                }
                (Some(side.upcast::<gtk::Widget>()), options)
            }
        };
        let card = content(n, requests.clone(), side.as_ref(), false);
        let menu = options.as_ref().map(|(_, menu)| menu.clone());
        if let Some((_, menu)) = &options {
            // Under its button, a gap from it, its right edge the button's (so it grows over
            // the card, never past it), as macOS's menus; it takes the keyboard while open
            // (Escape), as the bar's popovers. Weak: the menu lives in the window.
            menu.set_halign(gtk::Align::End);
            menu.set_offset(0, MENU_GAP);
            menu.connect_show(glib::clone!(
                #[weak]
                window,
                move |_| window.set_keyboard_mode(KeyboardMode::OnDemand)
            ));
            menu.connect_closed(glib::clone!(
                #[weak]
                window,
                move |_| window.set_keyboard_mode(KeyboardMode::None)
            ));
        }
        window.set_child(Some(&card));
        // GTK closes an open popover on a click outside it, and keeps the click: closed
        // first, by the pointer coming onto the banner, the click (on ×) reaches it. While
        // the popover holds its grab, the banner gets no motion, only the pointer's enter:
        // one soon after it maps is the banner showing under a still pointer (from then, not
        // from when it was built, which a slow host shows long after), which leaves it
        // open.
        let hover = gtk::EventControllerMotion::new();
        let shown = Rc::new(Cell::new(None));
        let first = shown.clone();
        window.connect_map(move |_| first.set(Some(Instant::now())));
        hover.connect_enter(move |_, _, _| {
            if shown
                .get()
                .is_some_and(|at: Instant| at.elapsed() > SETTLED)
            {
                crate::popover::close_open();
            }
        });
        card.add_controller(hover);
        // The banner follows a swipe sideways, as far as its anchored edge lets it.
        let (gap, swiped) = (self.gap, self.swipe);
        let moved = glib::clone!(
            #[weak]
            window,
            move |dx: f64| {
                window.set_opacity(1.0 - (dx.abs() / (2.0 * swiped)).min(0.8));
                match horizontal {
                    Some(Edge::Right) => window.set_margin(Edge::Right, gap - dx as i32),
                    Some(edge) => window.set_margin(edge, gap + dx as i32),
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
            countdown.follow(&card, menu.as_ref());
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

    /// The clock stopped while the pointer is over `card`, or its `menu` is open.
    fn follow(&self, card: &gtk::Overlay, menu: Option<&gtk::Popover>) {
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

/// The desktop entries Chromium-based browsers send (`desktop-entry`, Chromium's
/// `chrome::GetDesktopName`, less ".desktop").
const CHROMIUMS: [&str; 8] = [
    "brave-browser",
    "chromium",
    "chromium-browser",
    "google-chrome",
    "google-chrome-beta",
    "google-chrome-unstable",
    "microsoft-edge",
    "vivaldi-stable",
];

/// Chromium's own "Settings" action (key `settings`, its `kSettingsButtonId`), which it adds
/// to every web notification (the site's notification permissions): left out, from a
/// Chromium-based browser only.
fn chromium_settings(n: &Notification, key: &str) -> bool {
    key == "settings"
        && n.desktop_entry
            .as_deref()
            .is_some_and(|entry| CHROMIUMS.contains(&entry))
}

/// A notification's card, as macOS's: its app's icon (full colour, from its desktop file,
/// else the icon it sent), or its image with the app's icon as a badge on it; the summary,
/// with the time after it (`time`: in the Notification Centre) and its × (sending
/// `requests` its dismissal) in the time's place while hovered, the body (three lines at
/// most); `side` (its actions) under them, at the right.
pub fn content(
    n: &Notification,
    requests: UnboundedSender<Request>,
    side: Option<&gtk::Widget>,
    time: bool,
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
    // At the summary line's end: its time (in the Centre), and over it, in its place while
    // the card is hovered (or the × has the keyboard), the ×; as big as the larger.
    let slot = gtk::Overlay::builder()
        .css_classes(["slot"])
        .valign(gtk::Align::Start)
        .build();
    let stamp = match time {
        true => gtk::Label::new(Some(&ago(n.time))).upcast::<gtk::Widget>(),
        false => gtk::Box::new(gtk::Orientation::Horizontal, 0).upcast(),
    };
    stamp.add_css_class("time");
    slot.set_child(Some(&stamp));
    let close = close(n, requests);
    // Focusable in the Centre only: a banner's window never has the keyboard.
    close.set_focusable(time);
    slot.add_overlay(&close);
    slot.set_measure_overlay(&close, true);
    top.append(&slot);
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
    // The actions under the text, at its right: the summary line keeps the card's width,
    // its × at the card's top-right.
    if let Some(side) = side {
        side.set_halign(gtk::Align::End);
        text.append(side);
    }
    gtk::Overlay::builder()
        .child(&main)
        .css_classes(["card"])
        .build()
}

/// A notification's ×, which dismisses it; named after the summary (else its app) for screen
/// readers.
fn close(n: &Notification, requests: UnboundedSender<Request>) -> gtk::Button {
    let close = gtk::Button::builder()
        .icon_name("window-close-symbolic")
        .css_classes(["close"])
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();
    let id = n.id;
    let name = [&n.summary, &n.app]
        .into_iter()
        .find(|name| !name.trim().is_empty())
        .map_or("Dismiss".into(), |name| format!("Dismiss \"{name}\""));
    close.update_property(&[gtk::accessible::Property::Label(&name)]);
    close.connect_clicked(move |_| {
        let _ = requests.send(Request::Dismiss(id));
    });
    close
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

/// How long ago `time` (seconds since 1970) was: "now", "5m", "2h", "3d".
fn ago(time: u64) -> String {
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
