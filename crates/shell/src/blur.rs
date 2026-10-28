//! The compositor's blur behind a window, through ext-background-effect-v1 on GTK's own
//! `wl_surface`, from our own event queue on GDK's connection.
//!
//! GTK 4.23.3 and later bind the protocol themselves and create the effect object for every
//! surface (a second one is a protocol error); there, blur comes from blur nodes in the
//! window's snapshot instead, which is not built yet.

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::Context;
use gdk4_wayland::prelude::*;
use gdk4_wayland::{WaylandDisplay, WaylandSurface};
use gtk4::{self as gtk, gdk, glib, prelude::*};
use tracing::{debug, info, warn};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::{wl_compositor::WlCompositor, wl_region::WlRegion};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, delegate_noop};
use wayland_protocols::ext::background_effect::v1::client::{
    ext_background_effect_manager_v1::{self, Capability, ExtBackgroundEffectManagerV1},
    ext_background_effect_surface_v1::ExtBackgroundEffectSurfaceV1,
};

pub struct Blur {
    queue: RefCell<EventQueue<Capabilities>>,
    qh: QueueHandle<Capabilities>,
    manager: ExtBackgroundEffectManagerV1,
    compositor: WlCompositor,
}

type Effect = ExtBackgroundEffectSurfaceV1;

impl Blur {
    pub fn new(display: &gdk::Display) -> Option<Self> {
        let version = (
            gtk::major_version(),
            gtk::minor_version(),
            gtk::micro_version(),
        );
        if version >= (4, 23, 3) {
            info!("no blur: GTK's own background effect (GTK ≥ 4.23.3) is not supported yet");
            return None;
        }
        Self::connect(display).unwrap_or_else(|e| {
            warn!("no blur: {e:#}");
            None
        })
    }

    /// Without the protocol or its blur, `None` (logged here, as both are the compositor's
    /// choice, not an error).
    fn connect(display: &gdk::Display) -> anyhow::Result<Option<Self>> {
        let display = display
            .downcast_ref::<WaylandDisplay>()
            .and_then(|d| d.wl_display())
            .context("not a Wayland display")?;
        let backend = Proxy::backend(&display)
            .upgrade()
            .context("GDK's Wayland connection is gone")?;
        let connection = Connection::from_backend(backend);
        let (globals, mut queue) = registry_queue_init::<Capabilities>(&connection)?;
        let qh = queue.handle();
        let Ok(manager) = globals.bind(&qh, 1..=1, ()) else {
            info!("no blur: the compositor has no ext-background-effect-v1");
            return Ok(None);
        };
        let compositor = globals.bind(&qh, 1..=6, ())?;
        // The manager's capabilities follow its bind.
        let mut capabilities = Capabilities::default();
        queue.roundtrip(&mut capabilities)?;
        if !capabilities.blur {
            info!("no blur: the compositor offers none");
            return Ok(None);
        }
        Ok(Some(Self {
            queue: RefCell::new(queue),
            qh,
            manager,
            compositor,
        }))
    }

    /// Blur behind `native` (a bar's window, a popover, the OSD) while it is mapped: all of
    /// it, but for corners rounded at `radius` (at most half its height), following its
    /// size.
    pub fn attach(
        blur: &Option<Rc<Blur>>,
        native: &(impl IsA<gtk::Native> + IsA<gtk::Widget>),
        radius: i32,
    ) {
        let Some(blur) = blur.clone() else {
            return;
        };
        let effect: Rc<RefCell<Option<Effect>>> = Rc::default();
        // A rounded region follows the surface's layout (its size, and a popover's place,
        // which moves its arrow); a bar's covers all, whatever it is.
        if radius > 0 {
            native.connect_realize(glib::clone!(
                #[strong]
                blur,
                #[strong]
                effect,
                move |native| {
                    let Some(surface) = native.surface() else {
                        return;
                    };
                    let (blur, effect) = (blur.clone(), effect.clone());
                    let native = native.clone().upcast::<gtk::Widget>();
                    surface.connect_layout(move |_, _, _| {
                        if let Some(effect) = &*effect.borrow() {
                            blur.region(effect, &native, radius);
                        }
                    });
                }
            ));
        }
        native.connect_map(glib::clone!(
            #[strong]
            effect,
            move |native| {
                *effect.borrow_mut() = blur.apply(native.upcast_ref(), radius);
                // Bars once each; popovers and the OSD on every showing.
                if effect.borrow().is_some() && native.has_css_class("bar") {
                    info!("blur on");
                } else if effect.borrow().is_some() {
                    debug!("blur on, for a {}", native.type_().name());
                }
            }
        ));
        // Once the surface is gone the effect is inert; destroying it first is only tidy.
        native.connect_unmap(move |_| {
            if let Some(effect) = effect.take() {
                effect.destroy();
            }
        });
    }

    /// Blur behind `native`, which must be mapped; the request is applied with GTK's next
    /// commit.
    fn apply(&self, native: &gtk::Native, radius: i32) -> Option<Effect> {
        // Drains what came since (a later change of capabilities is not followed).
        if let Err(e) = self
            .queue
            .borrow_mut()
            .dispatch_pending(&mut Capabilities::default())
        {
            warn!("blur's Wayland queue: {e}");
        }
        let surface = native.surface()?;
        let wayland = surface.downcast_ref::<WaylandSurface>()?;
        let effect = self
            .manager
            .get_background_effect(&wayland.wl_surface()?, &self.qh, ());
        self.region(&effect, native.upcast_ref(), radius);
        Some(effect)
    }

    /// Set `effect`'s region to `native` as it is now: all of it, with its corners rounded,
    /// or for a popover its contents so, and its arrow.
    fn region(&self, effect: &Effect, native: &gtk::Widget, radius: i32) {
        let Some(surface) = native
            .downcast_ref::<gtk::Native>()
            .and_then(|n| n.surface())
        else {
            return;
        };
        let region = self.compositor.create_region(&self.qh, ());
        if radius == 0 {
            region.add(0, 0, i32::MAX, i32::MAX);
        }
        let popover = native.downcast_ref::<gtk::Popover>();
        let contents = popover.and_then(|p| p.first_child()?.compute_bounds(p));
        let (x, y, width, height) = match contents {
            Some(b) => (
                b.x() as i32,
                b.y() as i32,
                b.width() as i32,
                b.height() as i32,
            ),
            None => (0, 0, surface.width(), surface.height()),
        };
        let radius = radius.min(width / 2).min(height / 2).max(0);
        region.add(x, y + radius, width, height - 2 * radius);
        // The corners, two rows at a time.
        for (top, rows, inset) in corner_bands(radius) {
            region.add(x + inset, y + top, width - 2 * inset, rows);
            region.add(x + inset, y + height - top - rows, width - 2 * inset, rows);
        }
        if let Some(popover) = popover {
            for (left, top, wide) in arrow_rows(popover, (y, y + height), radius) {
                region.add(left, top, wide, 1);
            }
        }
        effect.set_blur_region(Some(&region));
        region.destroy();
    }
}

/// A popover's arrow, as GTK 4.22 draws it (`gtk_popover_get_gap_coords`): from its tip,
/// at the middle of the button it points to, to a base 24 px wide (kept clear of the
/// corners rounded at `radius`) on its contents' edge, `rows` (top, bottom) of the
/// contents. One row at a time: (left, row, width).
fn arrow_rows(
    popover: &gtk::Popover,
    (top, bottom): (i32, i32),
    radius: i32,
) -> Vec<(i32, i32, i32)> {
    let (Some(button), Some(popup)) = (
        popover.parent(),
        popover.surface().and_downcast::<gdk::Popup>(),
    ) else {
        return Vec::new();
    };
    let Some(native) = button.native() else {
        return Vec::new();
    };
    let Some(bounds) = button.compute_bounds(&native) else {
        return Vec::new();
    };
    let (nx, _) = native.surface_transform();
    let width = popover.width();
    let tip = (f64::from(bounds.x()) + nx).floor() as i32 + bounds.width().ceil() as i32 / 2
        - popup.position_x();
    // Like GTK's CLAMP, it never panics (the popover may not have its size yet).
    let clamp = |x: i32, low: i32, high: i32| x.max(low).min(high);
    let left = clamp(tip - 12, radius, width - 24 - radius);
    let right = clamp(tip + 12, radius + 24, width - radius);
    let tip = clamp(tip, 0, width);
    // Above the contents (a popover under its button), or below them.
    let (rows, tip_row, step) = match top > 0 {
        true => (top, 0, 1),
        false => (popover.height() - bottom, popover.height() - 1, -1),
    };
    (0..rows)
        .map(|i| {
            let along = |end: i32| tip + (end - tip) * (i + 1) / rows;
            (along(left), tip_row + step * i, along(right) - along(left))
        })
        .collect()
}

/// A corner rounded at `radius` as bands of two rows, top down: (first row, rows, how far
/// in the circle starts at the band's middle). Three bands showed as steps on the OSD's
/// pill; two rows do not.
fn corner_bands(radius: i32) -> Vec<(i32, i32, i32)> {
    let r = f64::from(radius);
    let band = |top: i32| {
        let rows = 2.min(radius - top);
        let dy = r - f64::from(top) - f64::from(rows) / 2.0;
        let inset = (r - (r * r - dy * dy).sqrt()).round() as i32;
        (top, rows, inset)
    };
    (0..radius).step_by(2).map(band).collect()
}

/// Whether the compositor can blur, from the manager's capabilities event.
#[derive(Default)]
pub struct Capabilities {
    blur: bool,
}

impl Dispatch<ExtBackgroundEffectManagerV1, ()> for Capabilities {
    fn event(
        state: &mut Self,
        _: &ExtBackgroundEffectManagerV1,
        event: ext_background_effect_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_background_effect_manager_v1::Event::Capabilities { flags } = event {
            state.blur = flags
                .into_result()
                .is_ok_and(|flags| flags.contains(Capability::Blur));
        }
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for Capabilities {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(Capabilities: WlCompositor);
delegate_noop!(Capabilities: WlRegion);
delegate_noop!(Capabilities: ExtBackgroundEffectSurfaceV1);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corners_narrow_towards_the_edge() {
        assert_eq!(corner_bands(0), Vec::new());
        let bands = corner_bands(10);
        assert_eq!(bands.len(), 5);
        assert_eq!((bands[0], bands[4]), ((0, 2, 6), (8, 2, 0)));
        assert_eq!(corner_bands(3), [(0, 2, 1), (2, 1, 0)]);
    }
}
