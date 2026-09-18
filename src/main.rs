//! A minimal Linux tray for the [mihomo] kernel.
//!
//! It talks to the kernel's external controller (`127.0.0.1:9090` by default,
//! `CLASH_CONTROLLER` and `CLASH_SECRET` override it) and keeps a fresh copy of
//! the kernel state for the StatusNotifierItem menu.
//!
//! [mihomo]: https://wiki.metacubex.one

mod api;
mod tray;

use std::process::exit;
use std::time::Duration;

use async_io::Timer;
use futures_channel::mpsc::{self, RecvError, UnboundedReceiver};
use futures_lite::future;
use ksni::{Handle, TrayMethods as _};

use crate::api::{Api, Command, Snapshot};
use crate::tray::{ClashTray, Shared};

/// How often the kernel is re-read when nothing else happens.
///
/// Opening the menu and every menu command refresh right away, so this only
/// decides how quickly changes made elsewhere (the dashboard, another client)
/// show up here.
const POLL: Duration = Duration::from_secs(30);

fn main() {
    async_io::block_on(run());
}

async fn run() {
    let api = Api::new(
        &env("CLASH_CONTROLLER").unwrap_or_else(|| "127.0.0.1:9090".to_owned()),
        env("CLASH_SECRET"),
    );

    let (commands, mut queue) = mpsc::unbounded();
    let state = Shared::new(Snapshot::fetch(&api).await);
    // Autostart runs us while the desktop is still coming up, so the tray host
    // usually does not exist yet. Assume it will: rather than failing right
    // away, this keeps the tray running and lets ksni register the item as soon
    // as the StatusNotifierWatcher appears.
    let tray = match ClashTray::new(state.clone(), commands)
        .assume_sni_available(true)
        .spawn()
        .await
    {
        Ok(tray) => tray,
        Err(error) => {
            eprintln!("clash-tray: {error}");
            exit(1);
        }
    };

    serve(&api, &state, &tray, &mut queue).await;
}

/// Poll the kernel, and run the commands the menu sends.
async fn serve(
    api: &Api,
    state: &Shared,
    tray: &Handle<ClashTray>,
    queue: &mut UnboundedReceiver<Command>,
) {
    loop {
        // Wake up on a menu command, or once the poll interval has passed.
        let Ok(command) = next_command(queue).await else {
            // The menu is gone, nobody is listening anymore.
            return;
        };

        // A command can fail, e.g. picking a node in a group that does not
        // support it; keep the reason so the menu can show it.
        let failure = match command {
            Some(command) => api.run(command).await.err().map(|error| error.to_string()),
            None => None,
        };
        refresh(api, state, tray, failure).await;
    }
}

/// The next menu command, or `None` once [`POLL`] has passed.
async fn next_command(
    queue: &mut UnboundedReceiver<Command>,
) -> Result<Option<Command>, RecvError> {
    future::or(async { queue.recv().await.map(Some) }, async {
        Timer::after(POLL).await;
        Ok(None)
    })
    .await
}

/// Read the kernel state and redraw the tray, reporting `failure` when the
/// command that triggered this refresh did not go through.
async fn refresh(api: &Api, state: &Shared, tray: &Handle<ClashTray>, failure: Option<String>) {
    let mut snapshot = Snapshot::fetch(api).await;
    snapshot.error = snapshot.error.or(failure);
    state.update(|state| *state = snapshot);

    // Redraw the tray; ksni only signals what actually changed.
    tray.update(|_| {}).await;
}

/// Read an environment variable, treating an empty value as unset.
fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}
