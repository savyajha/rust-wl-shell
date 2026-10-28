//! The default output's volume and mute, and the hardware outputs, over the PulseAudio
//! protocol (pipewire-pulse serves it), with libpulse.
//!
//! libpulse's main loop is not `Send`, so it runs on a thread of its own and everything
//! that touches it stays there: requests reach it through a channel and a byte on a
//! socket, which the loop watches as one of its own events. If the server goes away the
//! thread connects again every 2 s; meanwhile there is no audio.

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
use libpulse_binding::def::SinkFlagSet;
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
    /// The default output's name.
    pub default: String,
    /// The hardware outputs: filters and virtual sinks (EasyEffects') are left out.
    pub outputs: Vec<Output>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Output {
    pub name: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    /// In percent.
    Volume(u32),
    Mute(bool),
    /// The output to make the default, by name.
    Default(String),
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

/// A sink, as much as the snapshot needs.
#[derive(Clone, Debug, PartialEq)]
struct Sink {
    output: Output,
    volume: u32,
    muted: bool,
    hardware: bool,
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
        context.subscribe(InterestMaskSet::SINK | InterestMaskSet::SERVER, |_| {});
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
        let (state, sinks) = (state.clone(), RefCell::new(Vec::new()));
        let mut volumes = None;
        let listed = move |listed: ListResult<&SinkInfo>| match listed {
            ListResult::Item(info) => {
                let name = info.name.as_deref().unwrap_or("").to_string();
                if name == default {
                    volumes = Some(info.volume);
                }
                sinks.borrow_mut().push(Sink {
                    output: Output {
                        name,
                        description: info.description.as_deref().unwrap_or("").to_string(),
                    },
                    volume: percent(info.volume.max()),
                    muted: info.mute,
                    hardware: info.flags.contains(SinkFlagSet::HARDWARE),
                });
            }
            ListResult::End => {
                let audio = snapshot(&default, &sinks.take());
                let mut state = state.borrow_mut();
                state.volumes = volumes;
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
        Request::Default(name) => {
            context.set_default_sink(&name, |_| {});
        }
    }
}

/// libpulse's volume in percent, rounded.
fn percent(volume: Volume) -> u32 {
    let normal = u64::from(Volume::NORMAL.0);
    ((u64::from(volume.0) * 100 + normal / 2) / normal) as u32
}

/// What the item shows, from the sinks and the default's name; `None` with no default.
fn snapshot(default: &str, sinks: &[Sink]) -> Option<Audio> {
    let sink = sinks.iter().find(|sink| sink.output.name == default)?;
    let outputs = sinks.iter().filter(|sink| sink.hardware);
    Some(Audio {
        volume: sink.volume,
        muted: sink.muted,
        default: default.to_string(),
        outputs: outputs.map(|sink| sink.output.clone()).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sink(name: &str, volume: u32, hardware: bool) -> Sink {
        Sink {
            output: Output {
                name: name.into(),
                description: format!("{name} output"),
            },
            volume,
            muted: false,
            hardware,
        }
    }

    #[test]
    fn the_default_sinks_volume_and_the_hardware_outputs() {
        let sinks = [
            sink("alsa_output.analog-stereo", 40, true),
            sink("easyeffects_sink", 100, false),
            sink("alsa_output.hdmi-stereo", 70, true),
        ];
        let audio = snapshot("alsa_output.analog-stereo", &sinks).unwrap();
        assert_eq!((audio.volume, audio.muted), (40, false));
        let names: Vec<&str> = audio.outputs.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(
            names,
            ["alsa_output.analog-stereo", "alsa_output.hdmi-stereo"]
        );
        // A virtual default (EasyEffects') still gives its volume.
        assert_eq!(snapshot("easyeffects_sink", &sinks).unwrap().volume, 100);
    }

    #[test]
    fn no_default_is_no_audio() {
        assert_eq!(snapshot("", &[sink("a", 10, true)]), None);
    }

    #[test]
    fn volumes_are_percent_of_normal() {
        assert_eq!(percent(Volume::NORMAL), 100);
        assert_eq!(percent(Volume(Volume::NORMAL.0 / 2)), 50);
        assert_eq!(percent(Volume(0)), 0);
        assert_eq!(percent(Volume(Volume::NORMAL.0 * 3 / 2)), 150);
    }
}
