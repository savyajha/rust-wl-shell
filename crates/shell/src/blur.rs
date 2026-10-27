//! The compositor's blur behind a window, through ext-background-effect-v1 on GTK's own
//! `wl_surface`, from our own event queue on GDK's connection.
//!
//! GTK 4.23.3 and later bind the protocol themselves and create the effect object for every
//! surface (a second one is a protocol error); there, blur comes from blur nodes in the
//! window's snapshot instead, which is not built yet.

use std::cell::RefCell;

use anyhow::Context;
use gdk4_wayland::prelude::*;
use gdk4_wayland::{WaylandDisplay, WaylandSurface};
use gtk4::{self as gtk, gdk, prelude::*};
use tracing::{info, warn};
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

pub type Effect = ExtBackgroundEffectSurfaceV1;

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

    /// Blur behind all of `window`, which must be realized; the request is applied with
    /// GTK's next commit. Once the surface is gone the effect is inert; destroying it first
    /// is only tidy.
    pub fn apply(&self, window: &gtk::Window) -> Option<Effect> {
        // Drains what came since (a later change of capabilities is not followed).
        if let Err(e) = self
            .queue
            .borrow_mut()
            .dispatch_pending(&mut Capabilities::default())
        {
            warn!("blur's Wayland queue: {e}");
        }
        let surface = window.surface()?.downcast::<WaylandSurface>().ok()?;
        let effect = self
            .manager
            .get_background_effect(&surface.wl_surface()?, &self.qh, ());
        let region = self.compositor.create_region(&self.qh, ());
        region.add(0, 0, i32::MAX, i32::MAX);
        effect.set_blur_region(Some(&region));
        region.destroy();
        info!("blur on");
        Some(effect)
    }
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
