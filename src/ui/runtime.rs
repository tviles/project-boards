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
    // Handlers first, so a signal arriving during start-up still takes the clean exit path.
    let signals = Signals {
        term: signal(SignalKind::terminate())?,
        hup: signal(SignalKind::hangup())?,
    };
    let mut terminal = ratatui::try_init()?;
    let _ = execute!(std::io::stdout(), EnableFocusChange);
    let result = event_loop(&mut terminal, options, herdr.clone(), signals).await;
    let _ = execute!(std::io::stdout(), DisableFocusChange);
    ratatui::restore();
    if let Err(e) = &result {
        // stderr may be a closed PTY under herdr; the log is the reliable record.
        tracing::error!(error = %e, "board pane failed");
    }
    // Close our own pane so no dead tab is left behind. herdr may already be closing it;
    // `pane_not_found` then is expected.
    if let Some(pane) = own_pane
        && let Err(e) = herdr.call(&["pane".into(), "close".into(), pane])
        && e.code != "pane_not_found"
    {
        tracing::warn!(error = %e, "could not close the board pane");
    }
    result
}

struct Signals {
    term: tokio::signal::unix::Signal,
    hup: tokio::signal::unix::Signal,
}

type Tagged = (u64, crate::sync::SyncEvent);

async fn event_loop(
    terminal: &mut DefaultTerminal,
    options: PaneOptions,
    herdr: Arc<dyn HerdrCli>,
    signals: Signals,
) -> anyhow::Result<()> {
    let settings = StartSettings {
        max_items: options.config.max_items,
        gh_user: options.config.gh_user.clone(),
    };
    // Events arrive tagged with the generation of the job that produced them.
    let (tx, rx) = mpsc::unbounded_channel::<Tagged>();
    let mut controller = Controller::new(options, herdr, INCREMENTAL_MODE, Instant::now());
    let result = drive(terminal, &mut controller, tx, rx, &settings, signals).await;
    // Every exit path (Exit effect, SIGTERM/SIGHUP, a failed draw) unregisters the pane.
    controller.release();
    result
}

async fn drive(
    terminal: &mut DefaultTerminal,
    controller: &mut Controller,
    tx: mpsc::UnboundedSender<Tagged>,
    mut rx: mpsc::UnboundedReceiver<Tagged>,
    settings: &StartSettings,
    mut signals: Signals,
) -> anyhow::Result<()> {
    let mut runner: Option<Runner> = None;
    terminal.draw(|f| chrome::draw(f, &mut controller.app))?;
    let mut pending = start(controller, &mut runner, &tx, settings);
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
                        None => {
                            tracing::warn!(job = job.kind(), "no GitHub client yet; job dropped")
                        }
                    },
                    Effect::OpenUrl(url) => open_url(&url),
                    Effect::ResolveToken => {
                        pending.extend(start(controller, &mut runner, &tx, settings))
                    }
                    Effect::Exit => return Ok(()),
                }
            }
        }
        terminal.draw(|f| chrome::draw(f, &mut controller.app))?;
        let now = Instant::now();
        pending = tokio::select! {
            next = events.next() => match next {
                Some(Ok(Event::Key(key))) => controller.handle(Input::Key(key), now),
                Some(Ok(Event::FocusGained)) => controller.handle(Input::Focus(true), now),
                Some(Ok(Event::FocusLost)) => controller.handle(Input::Focus(false), now),
                Some(Ok(_)) => Vec::new(),
                // An input error is not transient: retrying would spin on the same error.
                Some(Err(e)) => return Err(anyhow::Error::new(e).context("reading terminal input")),
                // The stream ended (terminal gone): same as Exit.
                None => return Ok(()),
            },
            Some((generation, event)) = rx.recv() => controller.handle(Input::Sync { generation, event }, now),
            _ = tick.tick() => controller.handle(Input::Tick, now),
            _ = signals.term.recv() => return Ok(()),
            _ = signals.hup.recv() => return Ok(()),
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

/// What `start` needs from the config to build the GitHub client and sync jobs.
struct StartSettings {
    max_items: usize,
    gh_user: Option<String>,
}

/// Resolves the token, builds the GitHub client, and starts the controller.
fn start(
    controller: &mut Controller,
    runner: &mut Option<Runner>,
    tx: &mpsc::UnboundedSender<Tagged>,
    settings: &StartSettings,
) -> Vec<Effect> {
    match resolve_token_from_system(settings.gh_user.as_deref()) {
        Ok(token) => {
            let gh = Arc::new(Github::new(Arc::new(HttpTransport::new(token.value))));
            *runner = Some(Runner {
                gh,
                tx: tx.clone(),
                max_items: settings.max_items,
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
    // The controller already refuses these; checked again so nothing else can reach `open`.
    if !crate::ui::controller::is_web_link(url) {
        tracing::warn!("refused to open a link that is not an http(s) URL");
        return;
    }
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
