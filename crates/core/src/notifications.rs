//! The notification server (`org.freedesktop.Notifications` on the session bus): banners
//! to show, a history kept in a file under `$XDG_STATE_HOME`, and Do Not Disturb. The UI
//! gets the whole state on each change and sends back what the user does; this module
//! speaks the protocol. A banner leaving the screen is not a close: the notification
//! stays in the history, still to be acted on, as GNOME's do. A notification is closed
//! (`NotificationClosed`) when it is gone from both: dismissed, acted on, cleared, closed
//! by its app, or a transient one's banner gone.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::{env, fs};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tracing::{debug, info, warn};
use zbus::object_server::SignalEmitter;
use zbus::zvariant::OwnedValue;

use crate::Update;

const NAME: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
/// The timeout when the app gives none.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
const BANNERS: usize = 3;
const HISTORY: usize = 100;
/// The longest a summary and a body are kept, in bytes.
const SUMMARY: usize = 1024;
const BODY: usize = 4096;
/// The largest image kept, a side in pixels.
const IMAGE: i32 = 2048;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    pub id: u32,
    /// The app's name, and where in it the notification is from (KDE's origin hint).
    pub app: String,
    /// An icon name, or a file's path; empty for none.
    pub icon: String,
    /// The app's desktop file's name, without `.desktop` (the `desktop-entry` hint).
    #[serde(default)]
    pub desktop_entry: Option<String>,
    pub summary: String,
    /// In the spec's markup (b, i, u, a).
    pub body: String,
    /// When it came, in seconds since 1970.
    pub time: u64,
    #[serde(skip)]
    pub image: Option<Image>,
    /// (key, label) pairs; "default" is a click on the notification.
    #[serde(skip)]
    pub actions: Vec<(String, String)>,
    #[serde(skip)]
    pub critical: bool,
    /// How long its banner stays (paused while hovered); `None`: until dismissed.
    #[serde(skip)]
    pub timeout: Option<Duration>,
    #[serde(skip)]
    pub transient: bool,
}

/// An image sent in the notification's hints (`image-data`), as raw pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub width: i32,
    pub height: i32,
    pub rowstride: i32,
    pub alpha: bool,
    pub pixels: Arc<Vec<u8>>,
}

/// What the UI shows.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Notifications {
    /// Do Not Disturb: no banners, but for critical notifications.
    pub dnd: bool,
    /// Newest first, at most three.
    #[serde(skip)]
    pub banners: Vec<Notification>,
    /// Newest first.
    pub history: Vec<Notification>,
    /// The last id given, kept so that ids are not given again after a restart.
    #[serde(default)]
    pub next: u32,
}

/// What the user did.
pub enum Request {
    /// A banner's time is up.
    Expired(u32),
    /// Dismissed: × or a swipe, on a banner or in the history (an app's stack at once).
    Dismiss(Vec<u32>),
    /// An action (the "default" one for a click), with the activation token for the app.
    Action {
        id: u32,
        key: String,
        token: Option<String>,
    },
    ClearAll,
    Dnd(bool),
}

/// Why a notification closed, as the spec numbers them.
#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(u32)]
enum Reason {
    Expired = 1,
    Dismissed = 2,
    Closed = 3,
}

/// The notifications, and what changes them.
struct Store {
    state: Notifications,
    /// The longest a non-critical banner stays.
    cap: Duration,
}

impl Store {
    fn notify(
        &mut self,
        mut n: Notification,
        replaces: u32,
        timeout: i32,
    ) -> (u32, Vec<(u32, Reason)>) {
        let known = |id| {
            self.state
                .history
                .iter()
                .chain(&self.state.banners)
                .any(|n| n.id == id)
        };
        n.id = if replaces != 0 && known(replaces) {
            replaces
        } else {
            self.state.next += 1;
            self.state.next
        };
        n.timeout = match (timeout, n.critical) {
            // Critical: until dismissed, whatever the app asks.
            (_, true) => None,
            (..0, false) => Some(DEFAULT_TIMEOUT),
            // "Never": a non-critical banner still leaves; the history keeps it.
            (0, false) => Some(self.cap),
            (ms, _) => Some(Duration::from_millis(ms as u64)),
        };
        self.remove(n.id);
        let mut closed = Vec::new();
        if !n.transient {
            self.state.history.insert(0, n.clone());
            // Past the cap, the oldest goes; closed if it is not on screen either.
            let old: Vec<u32> = self
                .state
                .history
                .drain(HISTORY.min(self.state.history.len())..)
                .map(|n| n.id)
                .collect();
            let shown = |id: &u32| self.state.banners.iter().any(|n| n.id == *id);
            closed.extend(
                old.into_iter()
                    .filter(|id| !shown(id))
                    .map(|id| (id, Reason::Expired)),
            );
        }
        if !self.state.dnd || n.critical {
            self.state.banners.insert(0, n.clone());
        } else if n.transient {
            // Neither kept nor shown: done with at once.
            closed.push((n.id, Reason::Expired));
        }
        // Past three, the oldest banner leaves, as if its time were up.
        let gone: Vec<u32> = self
            .state
            .banners
            .iter()
            .skip(BANNERS)
            .map(|n| n.id)
            .collect();
        closed.extend(gone.into_iter().filter_map(|id| self.expire(id)));
        (n.id, closed)
    }

    /// A banner gone: closed if it was not kept (a transient one).
    fn expire(&mut self, id: u32) -> Option<(u32, Reason)> {
        let had = self.state.banners.len();
        self.state.banners.retain(|n| n.id != id);
        let kept = self.state.history.iter().any(|n| n.id == id);
        (self.state.banners.len() < had && !kept).then_some((id, Reason::Expired))
    }

    /// Gone from the banners and the history; whether it was there.
    fn remove(&mut self, id: u32) -> bool {
        let had = self.state.banners.len() + self.state.history.len();
        self.state.banners.retain(|n| n.id != id);
        self.state.history.retain(|n| n.id != id);
        self.state.banners.len() + self.state.history.len() < had
    }

    fn close(&mut self, id: u32, reason: Reason) -> Vec<(u32, Reason)> {
        if self.remove(id) {
            vec![(id, reason)]
        } else {
            Vec::new()
        }
    }

    fn clear(&mut self) -> Vec<(u32, Reason)> {
        let mut ids: Vec<u32> = self
            .state
            .history
            .iter()
            .chain(&self.state.banners)
            .map(|n| n.id)
            .collect();
        ids.sort();
        ids.dedup();
        self.state.banners.clear();
        self.state.history.clear();
        ids.into_iter().map(|id| (id, Reason::Dismissed)).collect()
    }
}

/// The D-Bus side: the store, shared with the UI's requests, and the updates.
struct Server {
    store: Arc<Mutex<Store>>,
    updates: UnboundedSender<Update>,
    file: PathBuf,
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Server {
    #[allow(clippy::too_many_arguments)]
    async fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> u32 {
        let n = notification(app_name, app_icon, summary, body, actions, &hints);
        let (id, closed) = self
            .store
            .lock()
            .unwrap()
            .notify(n, replaces_id, expire_timeout);
        debug!("notification {id} ({expire_timeout} ms asked)");
        self.changed(&emitter, Vec::new()).await;
        // Closed only after the reply: an app waits for its id before it listens for it
        // (a transient one under Do Not Disturb is closed at once).
        let emitter = emitter.to_owned();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            for (id, reason) in closed {
                let _ = Self::notification_closed(&emitter, id, reason as u32).await;
            }
        });
        id
    }

    async fn close_notification(
        &self,
        id: u32,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) {
        let closed = self.store.lock().unwrap().close(id, Reason::Closed);
        self.changed(&emitter, closed).await;
    }

    fn get_capabilities(&self) -> Vec<&str> {
        let capabilities = [
            "actions",
            "body",
            "body-markup",
            "icon-static",
            "persistence",
            "x-kde-origin-name",
        ];
        capabilities.to_vec()
    }

    fn get_server_information(&self) -> (&str, &str, &str, &str) {
        (
            "rust-wl-shell",
            "rust-wl-shell",
            env!("CARGO_PKG_VERSION"),
            "1.2",
        )
    }

    #[zbus(signal)]
    async fn notification_closed(
        emitter: &SignalEmitter<'_>,
        id: u32,
        reason: u32,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn action_invoked(
        emitter: &SignalEmitter<'_>,
        id: u32,
        action_key: &str,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn activation_token(
        emitter: &SignalEmitter<'_>,
        id: u32,
        activation_token: &str,
    ) -> zbus::Result<()>;
}

impl Server {
    /// After a change: the UI's update, the file, and the closed notifications' signals.
    async fn changed(&self, emitter: &SignalEmitter<'_>, closed: Vec<(u32, Reason)>) {
        let state = self.store.lock().unwrap().state.clone();
        if let Err(e) = save(&self.file, &state) {
            warn!("notifications: saving {}: {e}", self.file.display());
        }
        let _ = self.updates.send(Update::Notifications(state));
        for (id, reason) in closed {
            if let Err(e) = Self::notification_closed(emitter, id, reason as u32).await {
                warn!("notifications: {e}");
            }
        }
    }
}

/// Serve the notifications on the session bus, unless another server has the name, and
/// carry out the UI's requests. `cap` is the longest a non-critical banner stays.
pub async fn run(
    cap: Duration,
    updates: UnboundedSender<Update>,
    mut requests: UnboundedReceiver<Request>,
) {
    let file = state_file();
    let mut state = load(&file);
    state.banners.clear();
    // Files from before `next` was kept have only their ids.
    state.next = state
        .history
        .iter()
        .map(|n| n.id)
        .fold(state.next, u32::max);
    let _ = updates.send(Update::Notifications(state.clone()));
    let store = Arc::new(Mutex::new(Store { state, cap }));
    let server = Server {
        store: store.clone(),
        updates,
        file,
    };
    let connection = async {
        zbus::connection::Builder::session()?
            .name(NAME)?
            .serve_at(PATH, server)?
            .build()
            .await
    };
    let connection = match connection.await {
        Ok(connection) => connection,
        Err(e) => return info!("no notifications: {e}"),
    };
    let Ok(interface) = connection
        .object_server()
        .interface::<_, Server>(PATH)
        .await
    else {
        return;
    };
    while let Some(request) = requests.recv().await {
        let mut acted = None;
        let closed = {
            let mut store = store.lock().unwrap();
            match request {
                Request::Expired(id) => {
                    debug!("notification {id}: its banner's time is up");
                    store.expire(id).into_iter().collect()
                }
                Request::Dismiss(ids) => ids
                    .into_iter()
                    .flat_map(|id| store.close(id, Reason::Dismissed))
                    .collect(),
                Request::ClearAll => store.clear(),
                Request::Dnd(on) => {
                    store.state.dnd = on;
                    Vec::new()
                }
                // Acted on, it is done with.
                Request::Action { id, key, token } => {
                    let closed = store.close(id, Reason::Dismissed);
                    acted = (!closed.is_empty()).then_some((id, key, token));
                    closed
                }
            }
        };
        let emitter = interface.signal_emitter();
        if let Some((id, key, token)) = acted {
            if let Some(token) = token {
                let _ = Server::activation_token(emitter, id, &token).await;
            }
            let _ = Server::action_invoked(emitter, id, &key).await;
        }
        interface.get().await.changed(emitter, closed).await;
    }
}

/// A notification from `Notify`'s arguments.
fn notification(
    app: String,
    icon: String,
    summary: String,
    body: String,
    actions: Vec<String>,
    hints: &HashMap<String, OwnedValue>,
) -> Notification {
    let hint = |names: &[&str]| {
        names
            .iter()
            .find_map(|name| hints.get(*name)?.try_clone().ok())
    };
    let origin = hint(&["x-kde-origin-name"]).and_then(|v| String::try_from(v).ok());
    let desktop_entry = hint(&["desktop-entry"]).and_then(|v| String::try_from(v).ok());
    let critical = hint(&["urgency"]).and_then(|v| u8::try_from(v).ok()) == Some(2);
    // A boolean, or (as some apps send it) an integer.
    let transient = hints.get("transient").is_some_and(|v| {
        v.downcast_ref::<bool>().unwrap_or(false) || v.downcast_ref::<i32>().is_ok_and(|i| i != 0)
    });
    let image = hint(&["image-data", "image_data", "icon_data"])
        .and_then(|v| image(<(i32, i32, i32, bool, i32, i32, Vec<u8>)>::try_from(v).ok()?));
    let image_path = hint(&["image-path", "image_path"]).and_then(|v| String::try_from(v).ok());
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    Notification {
        app: match origin {
            Some(origin) if !origin.is_empty() => format!("{app} \u{2014} {origin}"),
            _ => app,
        },
        icon: image_path.filter(|_| icon.is_empty()).unwrap_or(icon),
        desktop_entry: desktop_entry.filter(|e| !e.is_empty()),
        summary: cut(summary, SUMMARY),
        body: cut(without_links(&body), BODY),
        time,
        image,
        actions: actions
            .as_chunks::<2>()
            .0
            .iter()
            .map(|[key, label]| (key.clone(), label.clone()))
            .collect(),
        critical,
        transient,
        ..Default::default()
    }
}

/// An `image-data` hint's image (width, height, rowstride, alpha, bits per sample,
/// channels, pixels), if it is one GTK can take: 8 bits RGB or RGBA, at most 2048 px a
/// side, its rows as long as it says and all there. Else none: GTK would fail on it.
fn image(
    (width, height, rowstride, alpha, bits, channels, pixels): (
        i32,
        i32,
        i32,
        bool,
        i32,
        i32,
        Vec<u8>,
    ),
) -> Option<Image> {
    let fits = (1..=IMAGE).contains(&width) && (1..=IMAGE).contains(&height);
    if !fits || bits != 8 || channels != 3 + i32::from(alpha) {
        return None;
    }
    // Small enough now not to overflow.
    let row = width * channels;
    let rows = (height as i64 - 1) * i64::from(rowstride) + i64::from(row);
    (rowstride >= row && pixels.len() as i64 >= rows).then(|| Image {
        width,
        height,
        rowstride,
        alpha,
        pixels: Arc::new(pixels),
    })
}

/// The body without its links' tags (`<a href=...>` and `</a>`), their text kept: Pango's
/// markup has no links, and some apps send them though the server does not offer them.
fn without_links(body: &str) -> String {
    let mut text = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(start) = rest.find('<') {
        // `<a` or `</a`, in either case, then a space or the tag's end; not `<abbr>`.
        let name = &rest[start + 1..];
        let name = name.strip_prefix('/').unwrap_or(name);
        let after = name.strip_prefix(['a', 'A']);
        let link =
            after.is_some_and(|after| after.starts_with(|c: char| c == '>' || c.is_whitespace()));
        match rest[start..].find('>').filter(|_| link) {
            Some(end) => {
                text.push_str(&rest[..start]);
                rest = &rest[start + end + 1..];
            }
            // Not a link's tag, or one never closed: kept as it is.
            None => {
                text.push_str(&rest[..=start]);
                rest = &rest[start + 1..];
            }
        }
    }
    text.push_str(rest);
    text
}

/// `text` cut to at most `max` bytes, on a character's boundary.
fn cut(text: impl Into<String>, max: usize) -> String {
    let mut text = text.into();
    if text.len() > max {
        let end = (0..=max)
            .rev()
            .find(|&i| text.is_char_boundary(i))
            .unwrap_or(0);
        text.truncate(end);
    }
    text
}

fn state_file() -> PathBuf {
    let home = || PathBuf::from(env::var_os("HOME").unwrap_or_default()).join(".local/state");
    let dir = env::var_os("XDG_STATE_HOME").map_or_else(home, PathBuf::from);
    dir.join("rust-wl-shell/notifications.json")
}

/// The saved history and Do Not Disturb; none if there is no file, or it does not read.
fn load(file: &PathBuf) -> Notifications {
    let read = fs::read(file)
        .ok()
        .and_then(|data| serde_json::from_slice(&data).ok());
    read.unwrap_or_default()
}

fn save(file: &PathBuf, state: &Notifications) -> std::io::Result<()> {
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir)?;
    }
    let partial = file.with_extension("json.partial");
    fs::write(&partial, serde_json::to_vec(state)?)?;
    fs::rename(partial, file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        let state = Notifications::default();
        let cap = Duration::from_secs(10);
        Store { state, cap }
    }

    fn note(summary: &str) -> Notification {
        Notification {
            app: "App".into(),
            summary: summary.into(),
            ..Default::default()
        }
    }

    fn ids(list: &[Notification]) -> Vec<u32> {
        list.iter().map(|n| n.id).collect()
    }

    #[test]
    fn timeouts() {
        let mut s = store();
        let critical = Notification {
            critical: true,
            ..note("c")
        };
        let timeout = |s: &mut Store, n: Notification, ms| {
            let (id, _) = s.notify(n, 0, ms);
            s.state.history.iter().find(|n| n.id == id).unwrap().timeout
        };
        assert_eq!(timeout(&mut s, note("a"), -1), Some(DEFAULT_TIMEOUT));
        // "Never": capped, but for critical notifications.
        assert_eq!(timeout(&mut s, note("b"), 0), Some(Duration::from_secs(10)));
        assert_eq!(timeout(&mut s, critical.clone(), 0), None);
        assert_eq!(timeout(&mut s, critical.clone(), -1), None);
        // Critical stays, even with a timeout of the app's.
        assert_eq!(timeout(&mut s, critical, 3000), None);
        assert_eq!(
            timeout(&mut s, note("d"), 2500),
            Some(Duration::from_millis(2500))
        );
    }

    #[test]
    fn banners_newest_first_three_at_most() {
        let mut s = store();
        let mut closed = Vec::new();
        for summary in ["1", "2", "3", "4"] {
            closed.extend(s.notify(note(summary), 0, -1).1);
        }
        assert_eq!(ids(&s.state.banners), [4, 3, 2]);
        assert_eq!(ids(&s.state.history), [4, 3, 2, 1]);
        // The first is still in the history: no close.
        assert!(closed.is_empty());
        // A transient one pushed out is gone, so closed as expired.
        let transient = Notification {
            transient: true,
            ..note("t")
        };
        s.notify(transient, 0, -1);
        for summary in ["6", "7", "8"] {
            closed.extend(s.notify(note(summary), 0, -1).1);
        }
        assert_eq!(closed, [(5, Reason::Expired)]);
    }

    #[test]
    fn replacing_and_closing() {
        let mut s = store();
        let (id, _) = s.notify(note("old"), 0, -1);
        assert_eq!(s.notify(note("new"), id, -1).0, id);
        assert_eq!(s.state.history.len(), 1);
        assert_eq!(s.state.history[0].summary, "new");
        // An unknown id to replace gets a new one.
        assert_eq!(s.notify(note("other"), 99, -1).0, id + 1);
        // Closed by its app: gone from the banners and the history.
        assert_eq!(s.close(id, Reason::Closed), [(id, Reason::Closed)]);
        assert!(!ids(&s.state.history).contains(&id) && !ids(&s.state.banners).contains(&id));
        assert_eq!(s.close(id, Reason::Closed), []);
        // A banner's time up: still in the history, not closed.
        assert_eq!(s.expire(id + 1), None);
        assert_eq!(ids(&s.state.history), [id + 1]);
        assert_eq!(s.clear(), [(id + 1, Reason::Dismissed)]);
    }

    #[test]
    fn do_not_disturb_keeps_banners_away_but_critical_ones() {
        let mut s = store();
        s.state.dnd = true;
        s.notify(note("quiet"), 0, -1);
        s.notify(
            Notification {
                critical: true,
                ..note("loud")
            },
            0,
            -1,
        );
        assert_eq!(ids(&s.state.banners), [2]);
        assert_eq!(ids(&s.state.history), [2, 1]);
    }

    #[test]
    fn the_history_and_dnd_are_saved_without_banners() {
        let dir = env::temp_dir().join(format!("rust-wl-shell-test-{}", std::process::id()));
        let file = dir.join("rust-wl-shell/notifications.json");
        let mut s = store();
        s.state.dnd = true;
        s.notify(
            Notification {
                critical: true,
                ..note("kept")
            },
            0,
            -1,
        );
        save(&file, &s.state).unwrap();
        let loaded = load(&file);
        assert!(loaded.dnd && loaded.banners.is_empty());
        assert_eq!(loaded.next, 1);
        assert_eq!(loaded.history[0].summary, "kept");
        fs::remove_dir_all(dir).unwrap();
        assert_eq!(load(&file), Notifications::default());
    }

    #[test]
    fn images_only_whole_and_of_a_kind_gtk_takes() {
        let pixels = |n| vec![0u8; n];
        // 2x2 RGBA, rows of 8 bytes; RGB with padded rows.
        assert!(image((2, 2, 8, true, 8, 4, pixels(16))).is_some());
        assert!(image((2, 2, 8, false, 8, 3, pixels(14))).is_some());
        for bad in [
            (2, 2, 8, true, 8, 4, pixels(15)),  // short
            (2, 2, 7, true, 8, 4, pixels(16)),  // rows shorter than a row
            (2, 2, 8, true, 16, 4, pixels(64)), // 16 bits
            (2, 2, 8, false, 8, 4, pixels(16)), // channels against alpha
            (0, 2, 8, true, 8, 4, pixels(16)),
            (4096, 1, 16384, true, 8, 4, pixels(16384)),
            (-1, -1, -1, true, 8, 4, pixels(0)),
        ] {
            assert!(image(bad.clone()).is_none(), "{:?}", bad.0);
        }
    }

    #[test]
    fn links_lose_their_tags_and_texts_are_cut() {
        let body = "See <a href=\"https://x.test\">the page</a> or <b>this</b>.";
        assert_eq!(without_links(body), "See the page or <b>this</b>.");
        assert_eq!(without_links("<a>bare</a><a href='x'"), "bare<a href='x'");
        assert_eq!(without_links("<A HREF='x'>Up</A>"), "Up");
        assert_eq!(without_links("<a\nhref='x'>Lines</a\n>"), "Lines");
        assert_eq!(
            without_links("<abbr>e.g.</abbr> <i>x</i>"),
            "<abbr>e.g.</abbr> <i>x</i>"
        );
        assert_eq!(cut("a\u{e9}b", 2), "a");
        assert_eq!(cut("abc", 10), "abc");
    }

    #[test]
    fn closed_when_gone_from_both() {
        // Transient under Do Not Disturb: neither shown nor kept.
        let mut s = store();
        s.state.dnd = true;
        let transient = Notification {
            transient: true,
            ..note("t")
        };
        assert_eq!(s.notify(transient, 0, -1).1, [(1, Reason::Expired)]);
        // Pushed out of the history while not on screen.
        let mut s = store();
        s.state.dnd = true;
        let mut closed = Vec::new();
        for i in 0..=HISTORY {
            closed.extend(s.notify(note(&i.to_string()), 0, -1).1);
        }
        assert_eq!(closed, [(1, Reason::Expired)]);
        assert_eq!(s.state.history.len(), HISTORY);
    }
}
