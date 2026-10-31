//! The password dialog, as macOS asks for a Wi-Fi password and styled as libadwaita's
//! alert dialog (in plain GTK, opaque, with no blur): a panel centred on the focused
//! output, on the overlay layer, which takes the keyboard while it is open. It asks for joining a network
//! from the Wi-Fi popover (and stays open, with the error, until the join succeeds or is
//! cancelled), and for NetworkManager's own requests, through the secret agent (answered
//! at once; NM cancelling one closes it). An enterprise network adds the username and a
//! disclosure with its method, inner method and domain. The password entry is emptied on
//! closing: the shell keeps no secret.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use gtk4::{self as gtk, gdk, glib, prelude::*};
use gtk4_layer_shell::{KeyboardMode, Layer, LayerShell};
use shell_core::agent::{self, Prompt, PromptKind, Secrets};
use shell_core::network::{self, Eap, Joined, Request, Secret, Security};
use shell_core::power::{self, Action};
use tokio::sync::mpsc::UnboundedSender;

pub struct Dialog {
    /// Its window, built afresh for each showing from `view`, and destroyed when it hides:
    /// a hidden window would be GTK's last toplevel once no bar is left, which its AT-SPI
    /// root crashes on (`Banners`); and widgets moved to a new window keep their
    /// accessibles realized, which GTK warns of.
    shown: RefCell<Option<Shown>>,
    view: RefCell<View>,
    asking: RefCell<Option<Asking>>,
    /// The focused output's name, as niri names it.
    focused: RefCell<Option<String>>,
    network: UnboundedSender<Request>,
    answers: UnboundedSender<agent::Answer>,
    power: UnboundedSender<power::Request>,
    /// A power action's countdown, while it runs.
    countdown: RefCell<Option<glib::SourceId>>,
}

/// What the dialog shows: plain state, never the password.
#[derive(Clone, Default)]
struct View {
    heading: String,
    body: String,
    /// The username entry, with its text (an identity NetworkManager has).
    username: Option<String>,
    password: bool,
    /// An enterprise network's method, inner method and domain, in "Options".
    options: bool,
    /// The default response's label; none for a dialog that only tells.
    join: Option<String>,
    error: String,
    /// Waiting for what it asked for (a join): its response insensitive.
    busy: bool,
}

/// A showing's window and the widgets read or updated while it is open.
struct Shown {
    window: gtk::Window,
    body: gtk::Label,
    username: gtk::Entry,
    password: gtk::PasswordEntry,
    method: gtk::DropDown,
    inner: gtk::DropDown,
    domain: gtk::Entry,
    error: gtk::Label,
    join: gtk::Button,
}

/// What the dialog is open for.
enum Asking {
    Join {
        ssid: String,
        security: Security,
    },
    Agent(u64),
    /// A power action: asked about (a restart, shut down or log out), or refused.
    Power(Action),
}

impl Dialog {
    pub fn new(
        network: UnboundedSender<Request>,
        answers: UnboundedSender<agent::Answer>,
        power: UnboundedSender<power::Request>,
    ) -> Rc<Self> {
        Rc::new(Self {
            shown: RefCell::default(),
            view: RefCell::default(),
            asking: RefCell::default(),
            focused: RefCell::default(),
            network,
            answers,
            power,
            countdown: RefCell::default(),
        })
    }

    /// Show `view` in a window built now on the focused output, with the keyboard; one
    /// already shown is kept, updated, its password focused again.
    fn show(self: &Rc<Self>) {
        let fresh = self.shown.borrow().is_none();
        if fresh {
            let shown = self.build();
            self.shown.replace(Some(shown));
        }
        self.update();
        let (shown, view) = (self.shown.borrow(), self.view.borrow());
        let Some(s) = shown.as_ref() else { return };
        s.window.set_keyboard_mode(KeyboardMode::OnDemand);
        s.window.present();
        if fresh && view.username.as_deref() == Some("") {
            s.username.grab_focus();
        } else if view.password {
            s.password.grab_focus();
        } else {
            s.join.grab_focus();
        }
    }

    fn build(self: &Rc<Self>) -> Shown {
        let view = self.view.borrow().clone();
        let label = |class: &str, text: &str| {
            let label = gtk::Label::builder()
                .label(text)
                .wrap(true)
                .justify(gtk::Justification::Center)
                .max_width_chars(30)
                .build();
            label.add_css_class(class);
            label
        };
        let (heading, body, error) = (
            label("heading", &view.heading),
            label("body", &view.body),
            label("error", ""),
        );
        let username = gtk::Entry::builder()
            .placeholder_text("Username")
            .text(view.username.as_deref().unwrap_or(""))
            .visible(view.username.is_some())
            .build();
        let password = gtk::PasswordEntry::builder()
            .placeholder_text("Password")
            .show_peek_icon(true)
            .visible(view.password)
            .build();
        let method = gtk::DropDown::from_strings(&["PEAP", "TTLS"]);
        let inner = gtk::DropDown::from_strings(&["MSCHAPv2", "PAP"]);
        let domain = gtk::Entry::builder().placeholder_text("Domain").build();
        let options_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        options_box.add_css_class("options");
        for (name, widget) in [
            ("Username", username.upcast_ref::<gtk::Widget>()),
            ("Password", password.upcast_ref()),
            ("Method", method.upcast_ref()),
            ("Inner method", inner.upcast_ref()),
            ("Domain", domain.upcast_ref()),
        ] {
            widget.update_property(&[gtk::accessible::Property::Label(name)]);
        }
        options_box.append(&method);
        options_box.append(&inner);
        options_box.append(&domain);
        let options = gtk::Expander::builder()
            .label("Options")
            .child(&options_box)
            .visible(view.options)
            .build();
        let (cancel, join) = (gtk::Button::with_label("Cancel"), gtk::Button::new());
        cancel.add_css_class("response");
        join.set_css_classes(&["response", "suggested"]);
        join.set_label(view.join.as_deref().unwrap_or(""));
        join.set_visible(view.join.is_some());
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        buttons.set_homogeneous(true);
        buttons.add_css_class("buttons");
        buttons.append(&cancel);
        buttons.append(&join);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.add_css_class("content");
        for widget in [
            heading.upcast_ref::<gtk::Widget>(),
            body.upcast_ref(),
            username.upcast_ref(),
            password.upcast_ref(),
            options.upcast_ref(),
            error.upcast_ref(),
            buttons.upcast_ref(),
        ] {
            content.append(widget);
        }
        let window = gtk::Window::builder()
            .css_classes(["dialog"])
            .child(&content)
            .build();
        window.init_layer_shell();
        window.set_namespace(Some("rust-wl-shell-dialog"));
        window.set_layer(Layer::Overlay);
        window.set_monitor(self.monitor().as_ref());
        // Opaque, as GNOME apps' dialogs: no blur behind it.
        let me = Rc::downgrade(self);
        let on = move |f: fn(&Rc<Dialog>)| {
            let me: Weak<Dialog> = me.clone();
            move || me.upgrade().iter().for_each(f)
        };
        // Enter joins, from any entry; Escape cancels.
        let (submit, cancelled) = (on(Dialog::submit), on(|d| d.cancel()));
        join.connect_clicked(move |_| submit());
        cancel.connect_clicked(move |_| cancelled());
        for entry in [&username, &domain] {
            let submit = on(Dialog::submit);
            entry.connect_activate(move |_| submit());
        }
        let submit = on(Dialog::submit);
        password.connect_activate(move |_| submit());
        let keys = gtk::EventControllerKey::new();
        let escape = on(|d| d.cancel());
        keys.connect_key_pressed(move |_, key, _, _| match key {
            gdk::Key::Escape => {
                escape();
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        });
        window.add_controller(keys);
        // Gone from the screen while counting down (its output unplugged): called off,
        // not to happen unseen; while NetworkManager asks, answered at once (not left to
        // its 2 min). Once GTK is done unmapping it: the window is destroyed
        // then, which inside the unmap (gtk4-layer-shell's remap) crashes. Not when it is
        // destroyed for hiding: no longer its window then.
        let me = Rc::downgrade(self);
        window.connect_unmap(move |window| {
            let (me, gone) = (me.clone(), window.downgrade());
            glib::idle_add_local_once(move || {
                let Some(dialog) = me.upgrade() else { return };
                let ours = matches!((gone.upgrade(), &*dialog.shown.borrow()),
                    (Some(w), Some(shown)) if w == shown.window);
                let agent = matches!(&*dialog.asking.borrow(), Some(Asking::Agent(_)));
                if ours && (agent || dialog.countdown.borrow().is_some()) {
                    dialog.cancel();
                }
            });
        });
        Shown {
            window,
            body,
            username,
            password,
            method,
            inner,
            domain,
            error,
            join,
        }
    }

    /// The window shown, if any, brought up to `view`'s body, error and response.
    fn update(&self) {
        let view = self.view.borrow();
        if let Some(shown) = &*self.shown.borrow() {
            shown.body.set_label(&view.body);
            shown.error.set_label(&view.error);
            shown.error.set_visible(!view.error.is_empty());
            shown.join.set_sensitive(!view.busy);
        }
    }

    /// The window destroyed, giving the keyboard back; what it showed is kept.
    fn hide(&self) {
        if let Some(shown) = self.shown.take() {
            shown.window.destroy();
        }
    }

    /// Where the dialog opens next.
    pub fn focus(&self, output: Option<String>) {
        self.focused.replace(output);
    }

    /// The focused output's monitor.
    fn monitor(&self) -> Option<gdk::Monitor> {
        let focused = self.focused.borrow().clone()?;
        let monitors = gdk::Display::default()?.monitors();
        let monitors = monitors.iter::<gdk::Monitor>().flatten();
        monitors
            .into_iter()
            .find(|m| m.connector().as_deref() == Some(focused.as_str()))
    }

    /// Ask for the password (or the credentials) to join `ssid`.
    /// A prompt already open is cancelled.
    pub fn ask_join(self: &Rc<Self>, ssid: String, security: Security) {
        self.cancel();
        let body = match security {
            Security::Wpa3 => {
                format!("The Wi-Fi network \u{201c}{ssid}\u{201d} requires a WPA3 password.")
            }
            Security::Enterprise => format!(
                "The Wi-Fi network \u{201c}{ssid}\u{201d} requires WPA2 enterprise credentials."
            ),
            _ => format!("The Wi-Fi network \u{201c}{ssid}\u{201d} requires a WPA2 password."),
        };
        let eap = security == Security::Enterprise;
        self.asking.replace(Some(Asking::Join { ssid, security }));
        self.open(View {
            heading: "Wi-Fi Password Required".into(),
            body,
            username: eap.then(String::new),
            options: eap,
            ..View::secret()
        });
    }

    /// Ask for what NetworkManager asks for. A prompt already open is cancelled.
    pub fn ask(self: &Rc<Self>, prompt: Prompt) {
        self.cancel();
        let name = &prompt.name;
        let (heading, body, username) = match &prompt.kind {
            PromptKind::Wifi => (
                "Wi-Fi Password Required",
                format!("The Wi-Fi network \u{201c}{name}\u{201d} requires a password."),
                None,
            ),
            PromptKind::Eap { identity } => (
                "Wi-Fi Password Required",
                format!("The Wi-Fi network \u{201c}{name}\u{201d} requires your credentials."),
                Some(identity.clone().unwrap_or_default()),
            ),
            PromptKind::Vpn => (
                "VPN Password Required",
                format!("The VPN \u{201c}{name}\u{201d} requires a password."),
                None,
            ),
        };
        self.asking.replace(Some(Asking::Agent(prompt.id)));
        self.open(View {
            heading: heading.into(),
            body,
            username,
            ..View::secret()
        });
    }

    /// Ask before a restart, a shut down or a log out, which comes by itself after
    /// `seconds`, showing what blocks it. A prompt already open is cancelled.
    pub fn ask_power(self: &Rc<Self>, action: Action, seconds: u32, blockers: Vec<String>) {
        self.cancel();
        let (heading, what) = match action {
            Action::Reboot => ("Restart?", "Your computer will restart"),
            Action::LogOut => ("Log Out?", "You will be logged out"),
            _ => ("Shut Down?", "Your computer will shut down"),
        };
        let blocked: String = blockers.iter().map(|b| format!("\n{b}")).collect();
        let blocked = match blocked.is_empty() {
            true => blocked,
            false => format!("\n\nBlocked by:{blocked}"),
        };
        let text = move |left: u32| {
            let unit = if left == 1 { "second" } else { "seconds" };
            format!("{what} in {left} {unit}.{blocked}")
        };
        self.asking.replace(Some(Asking::Power(action)));
        self.open(View {
            heading: heading.into(),
            body: text(seconds),
            join: Some(heading.replace('?', "")),
            ..View::default()
        });
        let (dialog, mut left) = (Rc::downgrade(self), seconds);
        let tick = glib::timeout_add_seconds_local(1, move || {
            let Some(dialog) = dialog.upgrade() else {
                return glib::ControlFlow::Break;
            };
            left = left.saturating_sub(1);
            dialog.view.borrow_mut().body = text(left);
            dialog.update();
            if left > 0 {
                return glib::ControlFlow::Continue;
            }
            dialog.countdown.take();
            dialog.submit();
            glib::ControlFlow::Break
        });
        self.countdown.replace(Some(tick));
    }

    /// A sleep starts, or the session locks: a power action asked about is called off.
    pub fn interrupted(&self) {
        if matches!(&*self.asking.borrow(), Some(Asking::Power(_))) {
            self.cancel();
        }
    }

    /// A power action done, or refused: its dialog closed, or shown again with logind's
    /// error; a refused lock or sleep (asked without a dialog) gets one, to say so.
    pub fn acted(self: &Rc<Self>, action: Action, error: Option<String>) {
        let ours = matches!(&*self.asking.borrow(), Some(Asking::Power(open)) if *open == action);
        if let (false, Some(_), Action::Lock | Action::Suspend) = (ours, &error, action) {
            let heading = if action == Action::Lock {
                "Not Locked"
            } else {
                "Not Asleep"
            };
            self.cancel();
            self.asking.replace(Some(Asking::Power(action)));
            self.view.replace(View {
                heading: heading.into(),
                ..View::default()
            });
        }
        // Not a dialog cancelled since: only a refused lock or sleep, which has none.
        let refused = error.is_some() && matches!(action, Action::Lock | Action::Suspend);
        self.finished(ours || refused, error.as_deref());
    }

    /// A join has ended.
    pub fn joined(self: &Rc<Self>, joined: &Joined) {
        let ours = matches!(&*self.asking.borrow(), Some(Asking::Join { ssid, .. }) if *ssid == joined.ssid);
        self.finished(ours, joined.error.as_deref());
    }

    /// What the dialog asked for has ended, if it is `ours`: done, it closes; else it is
    /// shown again (hidden while it ran), with the error.
    fn finished(self: &Rc<Self>, ours: bool, error: Option<&str>) {
        match error {
            _ if !ours => {}
            None => self.close(),
            Some(error) => {
                let mut view = self.view.borrow_mut();
                (view.error, view.busy) = (error.into(), false);
                drop(view);
                self.show();
            }
        }
    }

    /// NetworkManager cancelled prompt `id`.
    pub fn closed(&self, id: u64) {
        if matches!(&*self.asking.borrow(), Some(Asking::Agent(open)) if *open == id) {
            self.close();
        }
    }

    fn open(self: &Rc<Self>, view: View) {
        self.hide();
        self.view.replace(view);
        self.show();
    }

    fn submit(self: &Rc<Self>) {
        let read = self.shown.borrow().as_ref().map(|s| Eap {
            identity: s.username.text().into(),
            password: s.password.text().into(),
            peap: s.method.selected() == 0,
            mschapv2: s.inner.selected() == 0,
            domain: s.domain.text().into(),
        });
        match (&*self.asking.borrow(), read) {
            (Some(Asking::Join { ssid, security }), Some(eap)) => {
                let secret = match security {
                    Security::Enterprise => Secret::Eap(eap),
                    _ => Secret::Password(eap.password),
                };
                let ssid = ssid.clone();
                let _ = self.network.send(network::Request::Join { ssid, secret });
                let mut view = self.view.borrow_mut();
                (view.error, view.busy) = ("Joining\u{2026}".into(), true);
                drop(view);
                return self.update();
            }
            (Some(Asking::Agent(id)), Some(eap)) => {
                let (identity, password) = (eap.identity, eap.password);
                let _ = self
                    .answers
                    .send((*id, Some(Secrets { identity, password })));
            }
            // Hidden until it is done (polkit may ask, in a window of its own), or shown
            // again with its refusal.
            (Some(Asking::Power(action)), _) => {
                if let Some(tick) = self.countdown.take() {
                    tick.remove();
                }
                let _ = self.power.send(power::Request::Act(*action));
                self.view.borrow_mut().busy = true;
                return self.hide();
            }
            _ => {}
        }
        self.close();
    }

    fn cancel(&self) {
        if let Some(Asking::Agent(id)) = self.asking.take() {
            let _ = self.answers.send((id, None));
        }
        self.close();
    }

    fn close(&self) {
        if let Some(tick) = self.countdown.take() {
            tick.remove();
        }
        self.asking.replace(None);
        self.hide();
    }
}

impl View {
    /// A prompt for a secret: the password, and Join.
    fn secret() -> Self {
        Self {
            password: true,
            join: Some("Join".into()),
            ..Self::default()
        }
    }
}
