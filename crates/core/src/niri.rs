//! niri's IPC: its event stream, folded into the workspaces and the focused app, and the
//! actions the bar asks for.

use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use niri_ipc::state::{EventStreamState, EventStreamStatePart};
use niri_ipc::{Action, Event, Reply, Request, WorkspaceReferenceArg};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tracing::{debug, warn};

use crate::{Command, Update};

/// What the bars show from niri.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    /// Every output's workspaces, by output and then niri's index.
    pub workspaces: Vec<Workspace>,
    /// The focused window's app id.
    pub app_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Workspace {
    pub id: u64,
    /// The position on its output, from 1.
    pub idx: u8,
    pub name: Option<String>,
    pub output: Option<String>,
    /// Shown on its output.
    pub active: bool,
}

impl Snapshot {
    pub fn of(state: &EventStreamState) -> Self {
        let mut workspaces: Vec<Workspace> = state
            .workspaces
            .workspaces
            .values()
            .map(|ws| Workspace {
                id: ws.id,
                idx: ws.idx,
                name: ws.name.clone(),
                output: ws.output.clone(),
                active: ws.is_active,
            })
            .collect();
        workspaces.sort_by(|a, b| (&a.output, a.idx).cmp(&(&b.output, b.idx)));
        let focused = state.windows.windows.values().find(|w| w.is_focused);
        Self {
            workspaces,
            app_id: focused.and_then(|w| w.app_id.clone()),
        }
    }
}

/// Follow niri's event stream until it ends, and run `commands`. Returns only on an error:
/// niri's socket closing ends the shell.
pub async fn run(
    updates: UnboundedSender<Update>,
    mut commands: UnboundedReceiver<Command>,
) -> anyhow::Result<()> {
    let socket = PathBuf::from(env::var_os("NIRI_SOCKET").context("NIRI_SOCKET is not set")?);
    let mut events = request(&socket, Request::EventStream).await?;
    let mut state = EventStreamState::default();
    let mut shown = None;
    loop {
        tokio::select! {
            line = events.next_line() => {
                let line = line.context("reading niri's events")?;
                let line = line.context("niri closed its event stream")?;
                match serde_json::from_str::<Event>(&line) {
                    Ok(event) => {
                        state.apply(event);
                    }
                    // A newer niri's events, which this niri-ipc does not know.
                    Err(e) => debug!("unknown event from niri ({e}): {line}"),
                }
                let snapshot = Snapshot::of(&state);
                if shown.as_ref() != Some(&snapshot) {
                    shown = Some(snapshot.clone());
                    if updates.send(Update::Niri(snapshot)).is_err() {
                        return Ok(());
                    }
                }
            }
            Some(command) = commands.recv() => {
                let action = match command {
                    Command::FocusWorkspace(id) => Action::FocusWorkspace {
                        reference: WorkspaceReferenceArg::Id(id),
                    },
                };
                if let Err(e) = perform(&socket, action).await {
                    warn!("{e:#}");
                }
            }
        }
    }
}

type Lines = tokio::io::Lines<BufReader<UnixStream>>;

/// Send `request` on a new connection and read niri's reply; the lines after it, if any,
/// are left to the caller.
async fn request(socket: &Path, request: Request) -> anyhow::Result<Lines> {
    let mut stream = UnixStream::connect(socket)
        .await
        .with_context(|| format!("connecting to niri at {}", socket.display()))?;
    let mut line = serde_json::to_string(&request)?;
    line.push('\n');
    stream.write_all(line.as_bytes()).await?;
    let mut lines = BufReader::new(stream).lines();
    let reply = lines.next_line().await?.context("niri sent no reply")?;
    if let Err(e) = serde_json::from_str::<Reply>(&reply)? {
        bail!("niri refused {request:?}: {e}");
    }
    Ok(lines)
}

async fn perform(socket: &Path, action: Action) -> anyhow::Result<()> {
    request(socket, Request::Action(action)).await.map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// niri 26.04's event stream, recorded from a nested niri (one output, `winit`): its
    /// initial state; test.alpha opened, then test.beta; focus back to test.alpha; a
    /// switch to the empty workspace 2 and back; both windows closed.
    const RECORDED: &str = include_str!("../testdata/events.jsonl");

    /// The snapshot after each event.
    fn replay(lines: &str) -> Vec<Snapshot> {
        let mut state = EventStreamState::default();
        lines
            .lines()
            .map(|line| {
                state.apply(serde_json::from_str(line).unwrap());
                Snapshot::of(&state)
            })
            .collect()
    }

    /// The focused app and the active workspace's index, after each event.
    fn shown(snapshots: &[Snapshot]) -> Vec<(Option<&str>, Vec<u8>)> {
        snapshots
            .iter()
            .map(|s| {
                let active = s.workspaces.iter().filter(|w| w.active).map(|w| w.idx);
                (s.app_id.as_deref(), active.collect())
            })
            .collect()
    }

    #[test]
    fn workspaces_and_focus_follow_the_events() {
        let snapshots = replay(RECORDED);
        let (alpha, beta) = (Some("test.alpha"), Some("test.beta"));
        #[rustfmt::skip]
        let want = [
            (None, vec![1]), // WorkspacesChanged: one, empty
            (None, vec![1]), // WindowsChanged: none
            (None, vec![1]), (None, vec![1]), (None, vec![1]), (None, vec![1]), (None, vec![1]),
            (None, vec![1]), // WorkspacesChanged: a second, empty one
            (alpha, vec![1]), // WindowOpenedOrChanged
            (alpha, vec![1]), // WorkspaceActiveWindowChanged
            (beta, vec![1]), // WindowOpenedOrChanged
            (beta, vec![1]),
            (alpha, vec![1]), // WindowFocusChanged
            (alpha, vec![2]), // WorkspaceActivated 2
            (None, vec![2]), // WindowFocusChanged: none
            (None, vec![2]),
            (None, vec![1]), // WorkspaceActivated 1
            (alpha, vec![1]),
            (alpha, vec![1]),
            (beta, vec![1]),
            (beta, vec![1]), // WindowLayoutsChanged
            (beta, vec![1]), // WindowClosed: test.alpha
            (beta, vec![1]),
            (None, vec![1]), // WindowClosed: test.beta
            (None, vec![1]),
        ];
        assert_eq!(shown(&snapshots), want);
    }

    #[test]
    fn workspaces_are_ordered_and_keep_their_output() {
        let snapshots = replay(RECORDED);
        let workspaces = &snapshots[7].workspaces;
        let ids: Vec<(u64, u8)> = workspaces.iter().map(|w| (w.id, w.idx)).collect();
        assert_eq!(ids, [(1, 1), (2, 2)]);
        assert!(
            workspaces
                .iter()
                .all(|w| w.output.as_deref() == Some("winit"))
        );
    }

    #[test]
    fn workspaces_sort_by_output_then_index() {
        let event = r#"{"WorkspacesChanged":{"workspaces":[
            {"id":5,"idx":2,"name":null,"output":"B","is_urgent":false,"is_active":false,"is_focused":false,"active_window_id":null},
            {"id":3,"idx":1,"name":"web","output":"B","is_urgent":false,"is_active":true,"is_focused":true,"active_window_id":null},
            {"id":9,"idx":1,"name":null,"output":"A","is_urgent":false,"is_active":true,"is_focused":false,"active_window_id":null}
        ]}}"#;
        let snapshot = &replay(&event.replace('\n', ""))[0];
        let order: Vec<u64> = snapshot.workspaces.iter().map(|w| w.id).collect();
        assert_eq!(order, [9, 3, 5]);
        assert_eq!(snapshot.workspaces[1].name.as_deref(), Some("web"));
    }
}
