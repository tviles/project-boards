use crate::github::Github;
use crate::github::token::resolve_token_from_system;
use crate::github::transport::HttpTransport;
use crate::herdr::cli::{HerdrCli, ProcessHerdr};
use crate::sync::INCREMENTAL_MODE;
use crate::sync::syncer::Syncer;
use crate::ui::chrome;
use crate::ui::controller::{Controller, Effect, Input, PaneOptions, SyncJob};
use crossterm::event::{DisableFocusChange, EnableFocusChange, Event, EventStream};
use crossterm::execute;
use futures::StreamExt;
use ratatui::DefaultTerminal;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;

pub async fn run(options: PaneOptions) -> anyhow::Result<()> {
    let herdr: Arc<dyn HerdrCli> = Arc::new(ProcessHerdr::from_env());
    let own_pane = options.own_pane.clone();
    let mut terminal = ratatui::init();
    let _ = execute!(std::io::stdout(), EnableFocusChange);
    let result = event_loop(&mut terminal, options, herdr.clone()).await;
    let _ = execute!(std::io::stdout(), DisableFocusChange);
    ratatui::restore();
    // Close our own pane so no dead tab is left behind. herdr may already be closing it;
    // `pane_not_found` then is expected.
    if let Some(pane) = own_pane {
        if let Err(e) = herdr.call(&["pane".into(), "close".into(), pane]) {
            if e.code != "pane_not_found" {
                tracing::warn!(error = %e, "could not close the board pane");
            }
        }
    }
    result
}

type Tagged = (u64, crate::sync::SyncEvent);

async fn event_loop(
    terminal: &mut DefaultTerminal,
    options: PaneOptions,
    herdr: Arc<dyn HerdrCli>,
) -> anyhow::Result<()> {
    let max_items = options.config.max_items;
    // Events arrive tagged with the generation of the job that produced them.
    let (tx, rx) = mpsc::unbounded_channel::<Tagged>();
    let mut controller = Controller::new(options, herdr, INCREMENTAL_MODE, Instant::now());
    let result = drive(terminal, &mut controller, tx, rx, max_items).await;
    // Every exit path (Exit effect, SIGTERM/SIGHUP, a failed draw) unregisters the pane.
    controller.release();
    result
}

async fn drive(
    terminal: &mut DefaultTerminal,
    controller: &mut Controller,
    tx: mpsc::UnboundedSender<Tagged>,
    mut rx: mpsc::UnboundedReceiver<Tagged>,
    max_items: usize,
) -> anyhow::Result<()> {
    let mut runner: Option<Runner> = None;
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sighup = signal(SignalKind::hangup())?;
    terminal.draw(|f| chrome::draw(f, &mut controller.app))?;
    let mut pending = start(controller, &mut runner, &tx, max_items);
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        while !pending.is_empty() {
            for effect in std::mem::take(&mut pending) {
                match effect {
                    Effect::Spawn { job, generation } => match &runner {
                        // The effect carries the generation it was produced under; never read
                        // controller.generation() here (a bump may have happened since).
                        Some(r) => r.spawn(job, generation),
                        None => tracing::warn!(?job, "no GitHub client yet; job dropped"),
                    },
                    Effect::OpenUrl(url) => open_url(&url),
                    Effect::ResolveToken => {
                        pending.extend(start(controller, &mut runner, &tx, max_items))
                    }
                    Effect::Exit => return Ok(()),
                }
            }
        }
        terminal.draw(|f| chrome::draw(f, &mut controller.app))?;
        let now = Instant::now();
        pending = tokio::select! {
            Some(Ok(event)) = events.next() => match event {
                Event::Key(key) => controller.handle(Input::Key(key), now),
                Event::FocusGained => controller.handle(Input::Focus(true), now),
                Event::FocusLost => controller.handle(Input::Focus(false), now),
                _ => Vec::new(),
            },
            Some((generation, event)) = rx.recv() => controller.handle(Input::Sync { generation, event }, now),
            _ = tick.tick() => controller.handle(Input::Tick, now),
            _ = sigterm.recv() => return Ok(()),
            _ = sighup.recv() => return Ok(()),
        };
    }
}

/// Runs sync jobs. Every job gets its own `Syncer` whose events are forwarded tagged with the
/// controller generation the job was spawned under, so late events of a previous board are
/// dropped by `Controller::handle`.
struct Runner {
    gh: Arc<Github>,
    tx: mpsc::UnboundedSender<Tagged>,
    max_items: usize,
}

/// Resolves the token, builds the GitHub client, and starts the controller.
fn start(
    controller: &mut Controller,
    runner: &mut Option<Runner>,
    tx: &mpsc::UnboundedSender<Tagged>,
    max_items: usize,
) -> Vec<Effect> {
    match resolve_token_from_system() {
        Ok(token) => {
            let gh = Arc::new(Github::new(Arc::new(HttpTransport::new(token.value))));
            *runner = Some(Runner {
                gh,
                tx: tx.clone(),
                max_items,
            });
            controller.start(Ok(()), Instant::now())
        }
        Err(e) => controller.start(Err(e), Instant::now()),
    }
}

impl Runner {
    fn spawn(&self, job: SyncJob, generation: u64) {
        let (jtx, mut jrx) = mpsc::unbounded_channel();
        let out = self.tx.clone();
        tokio::spawn(async move {
            while let Some(event) = jrx.recv().await {
                if out.send((generation, event)).is_err() {
                    break;
                }
            }
        });
        let s = Syncer::new(self.gh.clone(), jtx, self.max_items, INCREMENTAL_MODE);
        tokio::spawn(async move {
            match job {
                SyncJob::Resolve(board) => {
                    if let Some(project) = s.resolve(&board).await {
                        s.full_load(&project.id).await;
                    }
                }
                SyncJob::Refresh(project) => {
                    // A failed refresh has already sent Failed { Resolve }, which ends the load
                    // for the controller; running full_load as well would double up.
                    if s.refresh_project(&project).await {
                        s.full_load(&project.id).await;
                    }
                }
                SyncJob::Incremental { project, since } => {
                    s.incremental(&project, &since).await;
                }
                SyncJob::ViewIds {
                    project,
                    view,
                    filter,
                    known,
                    hydrate,
                } => s.view_ids(&project, &view, &filter, &known, hydrate).await,
                SyncJob::Detail { item, before } => s.detail(&item, before).await,
                SyncJob::RepoProjects(repo) => s.repo_projects(&repo).await,
                SyncJob::Projects(repo) => s.projects(repo.as_ref()).await,
            }
        });
    }
}

fn open_url(url: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let spawned = std::process::Command::new(opener)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "could not open the browser");
    }
}
