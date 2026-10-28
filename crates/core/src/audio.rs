//! The default output's volume and mute, and the hardware outputs, over the PulseAudio
//! protocol (pipewire-pulse serves it), with libpulse.
//!
//! libpulse's main loop is not `Send`, so it runs on a thread of its own and everything
//! that touches it stays there: requests reach it through a channel and a byte on a
//! socket, which the loop watches as one of its own events. If the server goes away the
//! thread connects again every 2 s; meanwhile there is no audio.

use std::borrow::Cow;
use std::cell::RefCell;
use std::convert::Infallible;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::rc::{Rc, Weak};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use libpulse_binding::callbacks::ListResult;
use libpulse_binding::context::introspect::SinkInfo;
use libpulse_binding::context::subscribe::InterestMaskSet;
use libpulse_binding::context::{self, Context};
use libpulse_binding::def::{DevicePortType, PortAvailable, SinkFlagSet};
use libpulse_binding::mainloop::api::Mainloop as _;
use libpulse_binding::mainloop::events::io::FlagSet as IoEventFlagSet;
use libpulse_binding::mainloop::standard::{IterateResult, Mainloop};
use libpulse_binding::volume::{ChannelVolumes, Volume};
use tokio::sync::mpsc::WeakUnboundedSender;
use tracing::{debug, info};

use crate::Update;

const RETRY: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq)]
pub struct Audio {
    /// The default output's, in percent (above 100 when set so elsewhere).
    pub volume: u32,
    pub muted: bool,
    /// The default sink's name.
    pub default: String,
    /// The available ports of the hardware sinks, one output each (a sink without ports is
    /// one): filters and virtual sinks (EasyEffects') are left out.
    pub outputs: Vec<Output>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Output {
    pub sink: String,
    pub port: Option<String>,
    /// The port's description, and the card's when two outputs would share it.
    pub name: String,
    pub kind: Kind,
    /// The port its sink has now.
    pub port_active: bool,
    /// The default sink, on that port: what plays now.
    pub active: bool,
}

/// What an output is, for its icon.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Speaker,
    Headphones,
    Display,
    Bluetooth,
}

impl Audio {
    /// The output that plays now.
    pub fn active(&self) -> Option<&Output> {
        self.outputs.iter().find(|output| output.active)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    /// In percent.
    Volume(u32),
    Mute(bool),
    /// The sink to make the default, and its port to switch to (none: as it is).
    Default {
        sink: String,
        port: Option<String>,
    },
}

/// Requests for the audio thread; it wakes on each.
#[derive(Clone)]
pub struct Requests {
    queue: mpsc::Sender<Request>,
    wake: Arc<UnixStream>,
}

impl Requests {
    pub fn send(&self, request: Request) {
        // Both fail only once the thread is gone, and the shell with it.
        let _ = self.queue.send(request);
        let _ = (&*self.wake).write(&[0]);
    }
}

/// The audio thread's state, shared by libpulse's callbacks (all on that thread).
struct State {
    /// Weak, so that the shell's updates close when the core stops, with this thread on.
    updates: WeakUnboundedSender<Update>,
    requests: mpsc::Receiver<Request>,
    woken: UnixStream,
    shown: Option<Audio>,
    /// The default sink's channels, from the last read: a new level scales them, keeping
    /// their balance.
    volumes: Option<ChannelVolumes>,
    /// A sink or the server changed since the last read.
    changed: bool,
}

type Shared = Rc<RefCell<State>>;

impl State {
    /// Send `audio`; false once the shell's updates are closed.
    fn send(&self, audio: Option<Audio>) -> bool {
        let updates = self.updates.upgrade();
        updates.is_some_and(|updates| updates.send(Update::Audio(audio)).is_ok())
    }
}

/// Start the audio thread, which sends `Update::Audio` now and on each change.
pub fn start(updates: WeakUnboundedSender<Update>) -> Requests {
    let (queue, requests) = mpsc::channel();
    let (wake, woken) = UnixStream::pair().expect("a socket pair");
    woken.set_nonblocking(true).expect("a non-blocking socket");
    thread::Builder::new()
        .name("audio".into())
        .spawn(move || {
            let state = Rc::new(RefCell::new(State {
                updates,
                requests,
                woken,
                shown: None,
                volumes: None,
                changed: true,
            }));
            let mut last_error = None;
            loop {
                // Each error once while it repeats: the server may simply not run.
                let Err(e) = follow(&state);
                match last_error.replace(e) == Some(e) {
                    true => debug!("no audio: {e}"),
                    false => info!("no audio: {e}"),
                }
                let mut state = state.borrow_mut();
                state.shown = None;
                if !state.send(None) {
                    return;
                }
                drop(state);
                thread::sleep(RETRY);
            }
        })
        .expect("starting the audio thread");
    Requests {
        queue,
        wake: Arc::new(wake),
    }
}

/// Connect, then follow the server until the connection fails.
fn follow(state: &Shared) -> Result<Infallible, &'static str> {
    let mut mainloop = Mainloop::new().ok_or("no main loop")?;
    let context = Context::new(&mainloop, "rust-wl-shell").ok_or("no context")?;
    let context = Rc::new(RefCell::new(context));
    let connected = context
        .borrow_mut()
        .connect(None, context::FlagSet::NOAUTOSPAWN, None);
    connected.map_err(|_| "cannot connect")?;
    let ready = |context: &Rc<RefCell<Context>>| match context.borrow().get_state() {
        context::State::Ready => Ok(true),
        context::State::Failed | context::State::Terminated => Err("the server went away"),
        _ => Ok(false),
    };
    while !ready(&context)? {
        iterate(&mut mainloop)?;
    }
    info!("audio: connected");
    {
        // Requests from before, for a sink that may not be the default any more.
        let mut state = state.borrow_mut();
        while (&state.woken).read(&mut [0; 64]).is_ok_and(|n| n > 0) {}
        while state.requests.try_recv().is_ok() {}
        state.changed = true;
    }
    let weak = Rc::downgrade(&context);
    {
        let mut context = context.borrow_mut();
        let changed = Rc::downgrade(state);
        context.set_subscribe_callback(Some(Box::new(move |_, _, _| {
            if let Some(state) = changed.upgrade() {
                state.borrow_mut().changed = true;
            }
        })));
        // Cards too: a jack plugged in or out changes a port's availability.
        let interest = InterestMaskSet::SINK | InterestMaskSet::SERVER | InterestMaskSet::CARD;
        context.subscribe(interest, |_| {});
    }
    let on_wake = {
        let (state, context) = (state.clone(), weak.clone());
        move |_, _, _| {
            let state = state.borrow();
            while (&state.woken).read(&mut [0; 64]).is_ok_and(|n| n > 0) {}
            while let Ok(request) = state.requests.try_recv() {
                perform(&context, &state, request);
            }
        }
    };
    let fd = state.borrow().woken.as_raw_fd();
    let _wake = mainloop.new_io_event(fd, IoEventFlagSet::INPUT, Box::new(on_wake));
    loop {
        if std::mem::take(&mut state.borrow_mut().changed) {
            read(&weak, state);
        }
        iterate(&mut mainloop)?;
        ready(&context)?;
    }
}

fn iterate(mainloop: &mut Mainloop) -> Result<(), &'static str> {
    match mainloop.iterate(true) {
        IterateResult::Success(_) => Ok(()),
        _ => Err("the main loop stopped"),
    }
}

/// Read the default sink's name, then every sink, and send what changed.
fn read(context: &Weak<RefCell<Context>>, state: &Shared) {
    let Some(strong) = context.upgrade() else {
        return;
    };
    let (context, state) = (context.clone(), state.clone());
    strong.borrow().introspect().get_server_info(move |server| {
        let default = server
            .default_sink_name
            .as_deref()
            .unwrap_or("")
            .to_string();
        let Some(strong) = context.upgrade() else {
            return;
        };
        let state = state.clone();
        let (mut outputs, mut cards, mut playing) = (Vec::new(), Vec::new(), None);
        let listed = move |listed: ListResult<&SinkInfo>| match listed {
            ListResult::Item(info) => {
                let text = |s: &Option<Cow<str>>| s.as_deref().unwrap_or("").to_string();
                let sink = text(&info.name);
                if sink == default {
                    playing = Some((percent(info.volume.max()), info.mute, info.volume));
                }
                // pipewire-pulse copies its card's properties to a sink: the card's
                // description (under PulseAudio the sink's own), its bus, and its form
                // factor (PulseAudio's key, else PipeWire's), when the card has one.
                let property = |key: &str| info.proplist.get_str(key);
                let card = property("device.description").unwrap_or_default();
                let bus = property("device.bus");
                let ports = info.ports.iter().map(|p| Port {
                    name: text(&p.name),
                    description: text(&p.description),
                    kind: specific(p.r#type),
                    available: p.available != PortAvailable::No,
                });
                let sink = Sink {
                    name: sink,
                    card: card.clone(),
                    hardware: info.flags.contains(SinkFlagSet::HARDWARE),
                    bluetooth: bus.as_deref() == Some("bluetooth"),
                    kind: generic(
                        property("device.form_factor").or(property("device.form-factor")),
                        bus,
                    ),
                    ports: ports.collect(),
                    active_port: info.active_port.as_ref().map(|port| text(&port.name)),
                };
                for output in sink_outputs(sink, &default) {
                    outputs.push(output);
                    cards.push(card.clone());
                }
            }
            ListResult::End => {
                let mut outputs = std::mem::take(&mut outputs);
                dedupe(&mut outputs, &std::mem::take(&mut cards));
                let audio = playing.map(|(volume, muted, _)| Audio {
                    volume,
                    muted,
                    default: default.clone(),
                    outputs,
                });
                let mut state = state.borrow_mut();
                state.volumes = playing.take().map(|(_, _, volumes)| volumes);
                if state.shown != audio {
                    debug!("audio: {audio:?}");
                    state.send(audio.clone());
                    state.shown = audio;
                }
            }
            ListResult::Error => {}
        };
        strong.borrow().introspect().get_sink_info_list(listed);
    });
}

fn perform(context: &Weak<RefCell<Context>>, state: &State, request: Request) {
    let (Some(context), Some(shown)) = (context.upgrade(), &state.shown) else {
        return;
    };
    let mut context = context.borrow_mut();
    match request {
        Request::Volume(percent) => {
            let Some(mut volumes) = state.volumes else {
                return;
            };
            let raw = u64::from(Volume::NORMAL.0) * u64::from(percent.min(100)) / 100;
            volumes.scale(Volume(raw as u32));
            context
                .introspect()
                .set_sink_volume_by_name(&shown.default, &volumes, None);
        }
        Request::Mute(muted) => {
            context
                .introspect()
                .set_sink_mute_by_name(&shown.default, muted, None);
        }
        Request::Default { sink, port } => {
            // The port first, so that the sink plays on it once it is the default.
            if let Some(port) = port {
                context
                    .introspect()
                    .set_sink_port_by_name(&sink, &port, None);
            }
            context.set_default_sink(&sink, |_| {});
        }
    }
}

/// libpulse's volume in percent, rounded.
fn percent(volume: Volume) -> u32 {
    let normal = u64::from(Volume::NORMAL.0);
    ((u64::from(volume.0) * 100 + normal / 2) / normal) as u32
}

/// A sink, as libpulse lists it, as much as its outputs need.
struct Sink {
    name: String,
    /// Its card's description.
    card: String,
    hardware: bool,
    /// Bluetooth ports have generic descriptions ("Headphones"); the card is the device.
    bluetooth: bool,
    /// What its card is, for a generic port's icon.
    kind: Kind,
    ports: Vec<Port>,
    active_port: Option<String>,
}

struct Port {
    name: String,
    description: String,
    /// What its type says it is, when it says.
    kind: Option<Kind>,
    available: bool,
}

/// A hardware sink's outputs: one per available port, or one for the sink without ports;
/// `active` when it is the `default` sink, on that port. A port of a specific type is named
/// by its description ("Headphones"); a generic one (analog, line, unknown) by its card
/// ("Chu2 DSP"), with the card's kind. A Bluetooth sink's ports are named by its card
/// ("AirPods Pro"), with their own kind.
fn sink_outputs(sink: Sink, default: &str) -> Vec<Output> {
    if !sink.hardware {
        return Vec::new();
    }
    let available = sink.ports.into_iter().filter(|port| port.available);
    let mut ports: Vec<_> = available
        .map(|port| match port.kind {
            Some(kind) if sink.bluetooth => (Some(port.name), sink.card.clone(), kind),
            Some(kind) => (Some(port.name), port.description, kind),
            None => (Some(port.name), sink.card.clone(), sink.kind),
        })
        .collect();
    if ports.is_empty() {
        ports.push((None, sink.card.clone(), sink.kind));
    }
    let output = |(port, name, kind): (Option<String>, String, Kind)| {
        let port_active = port.is_none() || port == sink.active_port;
        let active = port_active && sink.name == default;
        let sink = sink.name.clone();
        Output {
            sink,
            port,
            name,
            kind,
            port_active,
            active,
        }
    };
    ports.into_iter().map(output).collect()
}

/// Names that outputs share get their card's description (`cards`, in step with them), if
/// it is not their name already; those still shared, " 2", " 3" in order.
fn dedupe(outputs: &mut [Output], cards: &[String]) {
    let names: Vec<String> = outputs.iter().map(|output| output.name.clone()).collect();
    for (i, output) in outputs.iter_mut().enumerate() {
        let shared = names.iter().filter(|name| **name == names[i]).count() > 1;
        if shared && names[i] != cards[i] {
            output.name = format!("{} ({})", names[i], cards[i]);
        }
    }
    let names: Vec<String> = outputs.iter().map(|output| output.name.clone()).collect();
    for (i, output) in outputs.iter_mut().enumerate() {
        let before = names[..i].iter().filter(|name| **name == names[i]).count();
        if before > 0 {
            output.name = format!("{} {}", names[i], before + 1);
        }
    }
}

/// A port's kind when its type is specific; none for a generic one (analog, line, S/PDIF,
/// unknown).
fn specific(kind: DevicePortType) -> Option<Kind> {
    match kind {
        DevicePortType::Speaker | DevicePortType::Earpiece => Some(Kind::Speaker),
        DevicePortType::Headphones | DevicePortType::Headset => Some(Kind::Headphones),
        DevicePortType::HDMI => Some(Kind::Display),
        DevicePortType::Bluetooth => Some(Kind::Bluetooth),
        _ => None,
    }
}

/// A card's kind, for its generic ports: by its form factor, else headphones for a USB or
/// Bluetooth one (a DAC, a headset), else speakers.
fn generic(form_factor: Option<String>, bus: Option<String>) -> Kind {
    match (form_factor.as_deref(), bus.as_deref()) {
        (Some("headphone" | "headset"), _) => Kind::Headphones,
        (Some("speaker"), _) => Kind::Speaker,
        (_, Some("usb" | "bluetooth")) => Kind::Headphones,
        _ => Kind::Speaker,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(name: &str) -> Output {
        Output {
            sink: String::new(),
            port: None,
            name: name.into(),
            kind: Kind::Speaker,
            port_active: true,
            active: false,
        }
    }

    /// A port: name, description, the kind its type gives, available.
    type P<'a> = (&'a str, &'a str, Option<Kind>, bool);

    fn sink(name: &str, card: &str, kind: Kind, ports: &[P], active: Option<&str>) -> Sink {
        let port = |&(name, description, kind, available): &P| Port {
            name: name.into(),
            description: description.into(),
            kind,
            available,
        };
        Sink {
            name: name.into(),
            card: card.into(),
            hardware: true,
            bluetooth: false,
            kind,
            ports: ports.iter().map(port).collect(),
            active_port: active.map(String::from),
        }
    }

    /// Each output's name, kind and whether it is active.
    fn shown(outputs: &[Output]) -> Vec<(&str, Kind, bool)> {
        outputs
            .iter()
            .map(|o| (o.name.as_str(), o.kind, o.active))
            .collect()
    }

    #[test]
    fn the_default_sinks_volume_and_the_hardware_outputs() {
        // The laptop's: its specific ports by name; the S/PDIF port is not available.
        let ports = [
            (
                "analog-output-speaker",
                "Speakers",
                Some(Kind::Speaker),
                true,
            ),
            (
                "analog-output-headphones",
                "Headphones",
                Some(Kind::Headphones),
                true,
            ),
            (
                "iec958-stereo-output",
                "Digital Output (S/PDIF)",
                None,
                false,
            ),
        ];
        let card = "Ryzen HD Audio Controller";
        let laptop = sink(
            "analog",
            card,
            Kind::Speaker,
            &ports,
            Some("analog-output-headphones"),
        );
        let outputs = sink_outputs(laptop, "analog");
        let want = [
            ("Speakers", Kind::Speaker, false),
            ("Headphones", Kind::Headphones, true),
        ];
        assert_eq!(shown(&outputs), want);
        assert!(outputs[1].port_active && !outputs[0].port_active);
        // The owner's USB DAC: a generic port, named by its card, a USB card's icon.
        let ports = [
            ("analog-output", "Analog Output", None, true),
            (
                "iec958-stereo-output",
                "Digital Output (S/PDIF)",
                None,
                false,
            ),
        ];
        let kind = generic(None, Some("usb".into()));
        let dac = sink("usb", "Chu2 DSP", kind, &ports, Some("analog-output"));
        assert_eq!(
            shown(&sink_outputs(dac, "analog")),
            [("Chu2 DSP", Kind::Headphones, false)]
        );
        // HDMI: a specific port; a sink without ports is one output, named by its card.
        let ports = [(
            "hdmi-output-0",
            "HDMI / DisplayPort",
            Some(Kind::Display),
            true,
        )];
        let hdmi = sink(
            "hdmi",
            "Renoir HDMI",
            Kind::Speaker,
            &ports,
            Some("hdmi-output-0"),
        );
        assert_eq!(
            shown(&sink_outputs(hdmi, "hdmi")),
            [("HDMI / DisplayPort", Kind::Display, true)]
        );
        // A Bluetooth headset: its route's generic description gives way to the device.
        let ports = [(
            "headphone-output",
            "Headphones",
            Some(Kind::Headphones),
            true,
        )];
        let mut airpods = sink("bluez", "AirPods Pro", Kind::Headphones, &ports, None);
        airpods.bluetooth = true;
        assert_eq!(
            shown(&sink_outputs(airpods, "x")),
            [("AirPods Pro", Kind::Headphones, false)]
        );
        let portless = sink_outputs(sink("p", "Card", Kind::Speaker, &[], None), "p");
        assert_eq!(shown(&portless), [("Card", Kind::Speaker, true)]);
        assert!(portless[0].port_active && portless[0].port.is_none());
        // A virtual sink (EasyEffects') is none.
        let mut virtual_sink = sink("easyeffects_sink", "Easy Effects", Kind::Speaker, &[], None);
        virtual_sink.hardware = false;
        assert!(sink_outputs(virtual_sink, "x").is_empty());
    }

    #[test]
    fn no_default_is_no_audio() {
        // No default sink: none is active (and the callback, finding no sink by that name
        // to take the volume from, sends no audio at all).
        let ports = [("speaker", "Speakers", Some(Kind::Speaker), true)];
        let laptop = sink("analog", "Card", Kind::Speaker, &ports, Some("speaker"));
        assert_eq!(
            shown(&sink_outputs(laptop, "")),
            [("Speakers", Kind::Speaker, false)]
        );
    }

    #[test]
    fn generic_ports_take_their_cards_kind() {
        let kind = |form: Option<&str>, bus: Option<&str>| {
            generic(form.map(String::from), bus.map(String::from))
        };
        assert_eq!(kind(Some("headset"), Some("pci")), Kind::Headphones);
        assert_eq!(kind(Some("speaker"), Some("usb")), Kind::Speaker);
        assert_eq!(kind(None, Some("bluetooth")), Kind::Headphones);
        assert_eq!(kind(None, Some("pci")), Kind::Speaker);
        assert_eq!(kind(None, None), Kind::Speaker);
        assert_eq!(specific(DevicePortType::Headphones), Some(Kind::Headphones));
        assert_eq!(specific(DevicePortType::Line), None);
    }

    #[test]
    fn shared_names_get_their_cards_then_numbers() {
        let mut outputs = [
            "Speakers", "Speakers", "HDMI", "Line Out", "Line Out", "Built-in", "Built-in",
        ]
        .map(output);
        let cards = [
            "Ryzen", "USB", "Ryzen", "Built-in", "Built-in", "Built-in", "Built-in",
        ]
        .map(String::from);
        dedupe(&mut outputs, &cards);
        let names: Vec<&str> = outputs.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Speakers (Ryzen)",
                "Speakers (USB)",
                "HDMI",
                "Line Out (Built-in)",
                "Line Out (Built-in) 2",
                // Named by their card already: numbered.
                "Built-in",
                "Built-in 2",
            ]
        );
    }

    #[test]
    fn volumes_are_percent_of_normal() {
        assert_eq!(percent(Volume::NORMAL), 100);
        assert_eq!(percent(Volume(Volume::NORMAL.0 / 2)), 50);
        assert_eq!(percent(Volume(0)), 0);
        assert_eq!(percent(Volume(Volume::NORMAL.0 * 3 / 2)), 150);
    }
}
