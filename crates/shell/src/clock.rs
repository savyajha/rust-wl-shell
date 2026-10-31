//! The clock, and its popover: Do Not Disturb, then the notifications' history, as macOS's
//! Notification Centre: grouped by app (the app with the newest first), newest first in
//! each group, each with × and swiped away as a banner is, and "Clear all".

use std::cell::RefCell;
use std::rc::Rc;

use gtk4::{self as gtk, prelude::*};
use shell_core::config::Config;
use shell_core::notifications::{Notification, Notifications, Request};
use tokio::sync::mpsc::UnboundedSender;

use crate::notifications::{Banners, act, content, swipe};
use crate::popover::{Item, Switch};

pub struct ClockItem {
    pub item: Item,
    pub label: gtk::Label,
    dnd: Switch,
    /// "Notifications" and "Clear all", then the groups.
    header: gtk::Box,
    groups: gtk::Box,
    empty: gtk::Label,
    shown: Option<Notifications>,
    requests: UnboundedSender<Request>,
    /// A swipe's distance to dismiss a notification.
    swipe: f64,
}

impl ClockItem {
    /// The clock; its popover only with `notifications` (the history's sender), the
    /// `banners` hidden while it is open.
    pub fn new(
        config: &Config,
        notifications: Option<UnboundedSender<Request>>,
        banners: Rc<RefCell<Banners>>,
    ) -> Self {
        let label = gtk::Label::new(None);
        label.add_css_class("clock");
        let item = Item::new(&label, "Clock");
        item.button.set_visible(true);
        let requests = notifications
            .clone()
            .unwrap_or_else(|| tokio::sync::mpsc::unbounded_channel().0);
        let sent = requests.clone();
        let dnd = Switch::new("Do Not Disturb", move |on| {
            let _ = sent.send(Request::Dnd(on));
        });
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        header.add_css_class("switch-row");
        let title = gtk::Label::builder()
            .label("Notifications")
            .xalign(0.0)
            .hexpand(true)
            .build();
        title.add_css_class("title");
        let clear = gtk::Button::builder()
            .label("Clear all")
            .css_classes(["clear"])
            .build();
        let sent = requests.clone();
        clear.connect_clicked(move |_| {
            let _ = sent.send(Request::ClearAll);
        });
        header.append(&title);
        header.append(&clear);
        let groups = gtk::Box::new(gtk::Orientation::Vertical, 0);
        groups.add_css_class("groups");
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(config.history_height as i32)
            .child(&groups)
            .build();
        let empty = gtk::Label::builder()
            .label("No notifications")
            .xalign(0.0)
            .build();
        empty.add_css_class("line");
        for widget in [
            dnd.row.upcast_ref::<gtk::Widget>(),
            header.upcast_ref(),
            empty.upcast_ref(),
            scrolled.upcast_ref(),
        ] {
            item.content.append(widget);
        }
        item.content.add_css_class("history");
        // The banners out of its way while it is open.
        let hidden = banners.clone();
        item.popover
            .connect_show(move |_| hidden.borrow_mut().hide(true));
        let shown = banners.clone();
        item.popover
            .connect_closed(move |_| shown.borrow_mut().hide(false));
        // Its bar gone while it is open (its output unplugged): not hidden for good.
        item.popover
            .connect_destroy(move |_| banners.borrow_mut().hide(false));
        // Without the server, the clock has no popover.
        if notifications.is_none() {
            item.button.set_popover(None::<&gtk::Popover>);
        }
        Self {
            item,
            label,
            dnd,
            header,
            groups,
            empty,
            shown: None,
            requests,
            swipe: f64::from(config.swipe_distance),
        }
    }

    pub fn show(&mut self, state: &Notifications) {
        self.dnd.show(state.dnd);
        if self
            .shown
            .as_ref()
            .is_some_and(|shown| shown.history == state.history)
        {
            return;
        }
        self.shown = Some(state.clone());
        self.header.set_visible(!state.history.is_empty());
        self.empty.set_visible(state.history.is_empty());
        while let Some(child) = self.groups.first_child() {
            self.groups.remove(&child);
        }
        for (app, notifications) in groups(&state.history) {
            let group = gtk::Box::new(gtk::Orientation::Vertical, 0);
            group.add_css_class("group");
            let title = gtk::Label::builder().label(app).xalign(0.0).build();
            title.add_css_class("title");
            group.append(&title);
            for n in notifications {
                group.append(&self.row(n));
            }
            self.groups.append(&group);
        }
    }

    /// A notification's row: its card, swiped away as a banner, a click on it its default
    /// action (when it still has one: not after a restart).
    fn row(&self, n: &Notification) -> gtk::Box {
        let card = content(n, self.requests.clone());
        card.add_css_class("row");
        let moved = follow_swipe(&card, self.swipe);
        let (requests, id) = (self.requests.clone(), n.id);
        swipe(&card, self.swipe, true, moved, move || {
            let _ = requests.send(Request::Dismiss(id));
        });
        if n.actions.iter().any(|(key, _)| key == "default") {
            let click = gtk::GestureClick::new();
            let (requests, popover) = (self.requests.clone(), self.item.popover.clone());
            click.connect_released(move |click, _, _, _| {
                if let Some(widget) = click.widget() {
                    act(&requests, id, "default", &widget);
                }
                popover.popdown();
            });
            card.add_controller(click);
        }
        card
    }
}

/// A row following a swipe: shifted by its margins, and fading.
fn follow_swipe(card: &gtk::Box, distance: f64) -> impl Fn(f64) + Clone + 'static {
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
