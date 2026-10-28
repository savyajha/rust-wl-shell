//! The password dialog, as macOS asks for a Wi-Fi password and styled as libadwaita's
//! alert dialog (in plain GTK): a frosted panel centred on the focused output, on the
//! overlay layer, which takes the keyboard while it is open. It asks for joining a network
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
use tokio::sync::mpsc::UnboundedSender;

use crate::blur::Blur;

pub struct Dialog {
    window: gtk::Window,
    heading: gtk::Label,
    body: gtk::Label,
    username: gtk::Entry,
    password: gtk::PasswordEntry,
    /// An enterprise network's method (PEAP, TTLS), inner method (MSCHAPv2, PAP) and
    /// domain.
    options: gtk::Expander,
    method: gtk::DropDown,
    inner: gtk::DropDown,
    domain: gtk::Entry,
    error: gtk::Label,
    join: gtk::Button,
    asking: RefCell<Option<Asking>>,
    /// The focused output's name, as niri names it.
    focused: RefCell<Option<String>>,
    network: UnboundedSender<Request>,
    answers: UnboundedSender<agent::Answer>,
}

/// What the dialog is open for.
enum Asking {
    Join { ssid: String, security: Security },
    Agent(u64),
}

impl Dialog {
    pub fn new(
        network: UnboundedSender<Request>,
        answers: UnboundedSender<agent::Answer>,
        blur: &Option<Rc<Blur>>,
    ) -> Rc<Self> {
        let window = gtk::Window::builder().css_classes(["dialog"]).build();
        window.init_layer_shell();
        window.set_namespace(Some("rust-wl-shell-dialog"));
        window.set_layer(Layer::Overlay);
        window.set_keyboard_mode(KeyboardMode::None);
        let label = |class: &str| {
            let label = gtk::Label::builder()
                .wrap(true)
                .justify(gtk::Justification::Center)
                .max_width_chars(30)
                .build();
            label.add_css_class(class);
            label
        };
        let (heading, body, error) = (label("heading"), label("body"), label("error"));
        let username = gtk::Entry::builder().placeholder_text("Username").build();
        let password = gtk::PasswordEntry::builder()
            .placeholder_text("Password")
            .show_peek_icon(true)
            .build();
        let method = gtk::DropDown::from_strings(&["PEAP", "TTLS"]);
        let inner = gtk::DropDown::from_strings(&["MSCHAPv2", "PAP"]);
        let domain = gtk::Entry::builder().placeholder_text("Domain").build();
        for (name, entry) in [
            ("Username", username.upcast_ref::<gtk::Widget>()),
            ("Password", password.upcast_ref()),
        ] {
            entry.update_property(&[gtk::accessible::Property::Label(name)]);
        }
        let options_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        for (name, widget) in [
            ("Method", method.upcast_ref::<gtk::Widget>()),
            ("Inner method", inner.upcast_ref()),
            ("Domain", domain.upcast_ref()),
        ] {
            widget.update_property(&[gtk::accessible::Property::Label(name)]);
            options_box.append(widget);
        }
        let options = gtk::Expander::builder()
            .label("Options")
            .child(&options_box)
            .build();
        let cancel = gtk::Button::builder()
            .label("Cancel")
            .css_classes(["pill"])
            .build();
        let join = gtk::Button::builder()
            .label("Join")
            .css_classes(["pill", "suggested"])
            .build();
        let buttons = gtk::Box::builder()
            .homogeneous(true)
            .spacing(12)
            .css_classes(["buttons"])
            .build();
        buttons.append(&cancel);
        buttons.append(&join);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
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
        window.set_child(Some(&content));
        Blur::attach(blur, &window, 14);
        let dialog = Rc::new(Self {
            window,
            heading,
            body,
            username,
            password,
            options,
            method,
            inner,
            domain,
            error,
            join,
            asking: RefCell::default(),
            focused: RefCell::default(),
            network,
            answers,
        });
        let weak = Rc::downgrade(&dialog);
        let on = move |f: fn(&Dialog)| {
            let weak: Weak<Dialog> = weak.clone();
            move || {
                if let Some(dialog) = weak.upgrade() {
                    f(&dialog)
                }
            }
        };
        let submit = on(Dialog::submit);
        dialog.join.connect_clicked(move |_| submit());
        let cancel_clicked = on(Dialog::cancel);
        cancel.connect_clicked(move |_| cancel_clicked());
        // Enter joins, from any entry; Escape cancels.
        let submit = on(Dialog::submit);
        dialog.password.connect_activate(move |_| submit());
        for entry in [&dialog.username, &dialog.domain] {
            let submit = on(Dialog::submit);
            entry.connect_activate(move |_| submit());
        }
        let keys = gtk::EventControllerKey::new();
        let escape = on(Dialog::cancel);
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Escape {
                escape();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        dialog.window.add_controller(keys);
        dialog
    }

    /// Where the dialog opens next.
    pub fn focus(&self, output: Option<String>) {
        self.focused.replace(output);
    }

    /// Ask for the password (or the credentials) to join `ssid`.
    /// A prompt already open is cancelled.
    pub fn ask_join(&self, ssid: String, security: Security) {
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
        self.open("Wi-Fi Password Required", &body, eap, None);
        self.options.set_visible(eap);
        self.asking.replace(Some(Asking::Join { ssid, security }));
    }

    /// Ask for what NetworkManager asks for. A prompt already open is cancelled.
    pub fn ask(&self, prompt: Prompt) {
        self.cancel();
        let name = &prompt.name;
        let (heading, body, identity) = match &prompt.kind {
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
        self.open(heading, &body, identity.is_some(), identity.as_deref());
        self.options.set_visible(false);
        self.asking.replace(Some(Asking::Agent(prompt.id)));
    }

    /// A join has ended: closed if it succeeded, else the error shown.
    pub fn joined(&self, joined: &Joined) {
        let ours = matches!(&*self.asking.borrow(), Some(Asking::Join { ssid, .. }) if *ssid == joined.ssid);
        if !ours {
            return;
        }
        match &joined.error {
            None => self.close(),
            Some(error) => {
                self.show_error(error);
                self.join.set_sensitive(true);
                self.password.grab_focus();
            }
        }
    }

    /// NetworkManager cancelled prompt `id`.
    pub fn closed(&self, id: u64) {
        if matches!(&*self.asking.borrow(), Some(Asking::Agent(open)) if *open == id) {
            self.close();
        }
    }

    fn open(&self, heading: &str, body: &str, username: bool, identity: Option<&str>) {
        self.heading.set_label(heading);
        self.body.set_label(body);
        self.username.set_visible(username);
        self.username.set_text(identity.unwrap_or(""));
        self.password.set_text("");
        self.show_error("");
        self.join.set_sensitive(true);
        let focused = self.focused.borrow().clone();
        let monitors = gdk::Display::default().map(|d| d.monitors());
        let monitor = monitors
            .iter()
            .flat_map(|m| m.iter::<gdk::Monitor>().flatten())
            .find(|m| focused.is_some() && m.connector().map(String::from) == focused);
        self.window.set_monitor(monitor.as_ref());
        self.window.set_keyboard_mode(KeyboardMode::OnDemand);
        self.window.present();
        if username && identity.unwrap_or("").is_empty() {
            self.username.grab_focus();
        } else {
            self.password.grab_focus();
        }
    }

    fn submit(&self) {
        let password = self.password.text().to_string();
        let identity = self.username.text().to_string();
        match &*self.asking.borrow() {
            Some(Asking::Join { ssid, security }) => {
                let secret = match security {
                    Security::Enterprise => Secret::Eap(Eap {
                        identity,
                        password,
                        peap: self.method.selected() == 0,
                        mschapv2: self.inner.selected() == 0,
                        domain: self.domain.text().to_string(),
                    }),
                    _ => Secret::Password(password),
                };
                let ssid = ssid.clone();
                let _ = self.network.send(network::Request::Join { ssid, secret });
                self.show_error("Joining\u{2026}");
                self.join.set_sensitive(false);
                return;
            }
            Some(Asking::Agent(id)) => {
                let _ = self
                    .answers
                    .send((*id, Some(Secrets { identity, password })));
            }
            None => {}
        }
        self.close();
    }

    /// The error line, hidden when empty.
    fn show_error(&self, text: &str) {
        self.error.set_label(text);
        self.error.set_visible(!text.is_empty());
    }

    fn cancel(&self) {
        if let Some(Asking::Agent(id)) = self.asking.take() {
            let _ = self.answers.send((id, None));
        }
        self.close();
    }

    fn close(&self) {
        self.asking.replace(None);
        self.password.set_text("");
        self.window.set_keyboard_mode(KeyboardMode::None);
        self.window.set_visible(false);
    }
}
