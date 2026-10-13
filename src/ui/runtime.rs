use crate::github::Github;
use crate::github::token::resolve_token_from_system;
use crate::github::transport::HttpTransport;
use crate::herdr::cli::{HerdrCli, ProcessHerdr};
use crate::sync::INCREMENTAL_MODE;
use crate::sync::syncer::Syncer;
use crate::ui::chrome;
use crate::ui::controller::{Controller, Effect, Input, PaneOptions, SyncJob};
use anyhow::Context;
use crossterm::cursor::Show;
use crossterm::event::{DisableFocusChange, EnableFocusChange, Event, EventStream};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};
use futures::{FutureExt, StreamExt};
use ratatui::DefaultTerminal;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::ThreadId;
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
        int: signal(SignalKind::interrupt())?,
    };
    let mut terminal = ratatui::try_init()?;
    keep_terminal_on_worker_panics(std::thread::current().id());
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

/// `ratatui::try_init` installs a panic hook that restores the terminal. A panic in a tokio
/// worker (a sync job; the syncer catches an edit's panic on purpose) must not do that while
/// the board keeps running, nor print over it: such panics are logged instead. A panic on the
/// UI thread still restores the terminal through the previous hook.
fn keep_terminal_on_worker_panics(ui: ThreadId) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if should_restore(std::thread::current().id(), ui) {
            previous(info);
        } else {
            let location = info
                .location()
                .map(|l| format!("{}:{}", l.file(), l.line()))
                .unwrap_or_default();
            tracing::error!(
                message = %panic_message(info.payload()),
                %location,
                "a background task panicked"
            );
        }
    }));
}

/// Whether a panic on thread `current` should restore the terminal: only one on the thread
/// that runs the UI (`ui`).
fn should_restore(current: ThreadId, ui: ThreadId) -> bool {
    current == ui
}

/// The text a panic was raised with.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload")
}

/// SIGTERM, SIGHUP and SIGINT take the clean exit path. In raw mode `Ctrl+C` is a key, so
/// SIGINT only comes from outside, or from `Ctrl+C` while `$EDITOR` has the terminal: that one
/// is the editor's and is dropped (`suspended_edit`).
struct Signals {
    term: tokio::signal::unix::Signal,
    hup: tokio::signal::unix::Signal,
    int: tokio::signal::unix::Signal,
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
        state_dir: options.state_dir.clone(),
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
    // Ticks missed while the editor ran are skipped, not fired in a burst.
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        while !pending.is_empty() {
            for effect in std::mem::take(&mut pending) {
                match effect {
                    Effect::Spawn { job, generation } => match &runner {
                        // The effect carries the generation it was produced under; never read
                        // controller.generation() here (a bump may have happened since).
                        Some(r) => r.spawn(job, generation),
                        None => {
                            tracing::warn!(job = job.kind(), "no GitHub client yet; job dropped");
                            // An edit must still be answered, or it would stay "sending".
                            if let Some(event) = unanswered_edit(&job) {
                                let _ = tx.send((generation, event));
                            }
                        }
                    },
                    Effect::OpenUrl(url) => open_url(&url),
                    Effect::ResolveToken => {
                        pending.extend(start(controller, &mut runner, &tx, settings))
                    }
                    Effect::Editor { purpose, initial } => {
                        let dir = settings.state_dir.join("edit");
                        let text = suspended_edit(terminal, &initial, &dir, &mut signals)?;
                        // A fresh reader: nothing typed into the editor reaches the board
                        // (pre-flight ruling 10; `suspended_edit` drained what was buffered).
                        events = EventStream::new();
                        let edited = Input::Edited { purpose, text };
                        pending.extend(controller.handle(edited, Instant::now()));
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
            _ = signals.int.recv() => return Ok(()),
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

/// What `start` needs from the config to build the GitHub client and sync jobs, and where
/// the editor's file goes.
struct StartSettings {
    max_items: usize,
    gh_user: Option<String>,
    state_dir: PathBuf,
}

/// Runs `$EDITOR` on `initial` with the TUI suspended, then restores it. `Ok(None)` when the
/// editor could not run or exited with an error; `Err` when the terminal could not be taken
/// back (the board then exits through the usual clean path).
///
/// The editor only ever starts from a key press, which the loop handles before it reads input
/// again, so no reader waits on the terminal meanwhile (Decision 27). Afterwards whatever
/// input is still buffered is dropped, never replayed on the board (pre-flight ruling 10),
/// and so is a SIGINT from `Ctrl+C` in the editor's terminal.
fn suspended_edit(
    terminal: &mut DefaultTerminal,
    initial: &str,
    dir: &Path,
    signals: &mut Signals,
) -> anyhow::Result<Option<String>> {
    let _ = execute!(std::io::stdout(), DisableFocusChange, Show);
    ratatui::restore();
    let command = crate::ui::editor::editor_command(|k| std::env::var(k).ok());
    let result =
        tokio::task::block_in_place(|| crate::ui::editor::edit_text(initial, &command, dir));
    // Not `ratatui::try_init`: it would install another panic hook over
    // `keep_terminal_on_worker_panics`, one that restores the terminal on any thread's panic.
    enable_raw_mode().context("taking the terminal back after the editor")?;
    execute!(std::io::stdout(), EnterAlternateScreen)
        .context("taking the terminal back after the editor")?;
    let _ = execute!(std::io::stdout(), EnableFocusChange);
    // The editor drew over the screen: the next draw repaints all of it.
    terminal.clear()?;
    drain_input();
    while let Some(Some(())) = signals.int.recv().now_or_never() {}
    match result {
        Ok(text) => Ok(text),
        Err(e) => {
            // The error names the failure only, never the text or the file's contents.
            tracing::warn!(error = %e, "could not run the editor");
            Ok(None)
        }
    }
}

/// Drops every input event already waiting: keys typed while the editor ran, or right after.
fn drain_input() {
    while crossterm::event::poll(Duration::ZERO).unwrap_or(false) {
        if crossterm::event::read().is_err() {
            break;
        }
    }
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
                SyncJob::Candidates(repo) => s.candidates(&repo).await,
                SyncJob::Repos => s.repos().await,
                SyncJob::LookupRepo(slug) => s.lookup_repo(&slug).await,
                SyncJob::Search { query, scope } => s.search(&query, &scope).await,
                SyncJob::Mutate { id, project, op } => s.mutate(id, &project, &op).await,
            }
        });
    }
}

/// The event that answers a job nobody could run (no token yet): `OpDone` for an edit, which
/// would otherwise stay "sending", and `Failed` for a lookup, which would otherwise stay
/// "loading" for good.
fn unanswered_edit(job: &SyncJob) -> Option<crate::sync::SyncEvent> {
    use crate::sync::{SyncEvent, SyncTask};
    let failed = |task| {
        Some(SyncEvent::Failed {
            task,
            error: crate::github::GithubError::NoToken,
        })
    };
    match job {
        SyncJob::Candidates(repo) => failed(SyncTask::Candidates(repo.clone())),
        SyncJob::Repos => failed(SyncTask::Repos),
        SyncJob::LookupRepo(slug) => failed(SyncTask::RepoLookup(slug.clone())),
        SyncJob::Search { query, .. } => failed(SyncTask::Search(query.clone())),
        SyncJob::Mutate { id, .. } => Some(crate::sync::SyncEvent::OpDone {
            id: *id,
            result: Err(crate::github::GithubError::NoToken),
        }),
        _ => None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FieldId, ItemId, ProjectId};
    use crate::ops::Op;
    use crate::sync::SyncEvent;

    #[test]
    fn only_a_panic_on_the_ui_thread_restores_the_terminal() {
        let ui = std::thread::current().id();
        assert!(should_restore(ui, ui));
        let worker = std::thread::spawn(|| std::thread::current().id())
            .join()
            .unwrap();
        assert!(!should_restore(worker, ui));
    }

    #[test]
    fn a_panic_message_is_read_from_str_and_string_payloads() {
        assert_eq!(panic_message(&"boom"), "boom");
        assert_eq!(panic_message(&String::from("bang")), "bang");
        assert_eq!(panic_message(&42_u8), "non-string panic payload");
    }

    #[test]
    fn an_edit_with_no_client_is_answered_with_an_error() {
        let job = SyncJob::Mutate {
            id: 3,
            project: ProjectId::new("P"),
            op: Op::ClearField {
                item: ItemId::new("I"),
                field: FieldId::new("F"),
            },
        };
        assert!(matches!(
            unanswered_edit(&job),
            Some(SyncEvent::OpDone {
                id: 3,
                result: Err(_)
            })
        ));
        assert!(unanswered_edit(&SyncJob::Projects(None)).is_none());
        assert!(matches!(
            unanswered_edit(&SyncJob::Repos),
            Some(crate::sync::SyncEvent::Failed {
                task: crate::sync::SyncTask::Repos,
                ..
            })
        ));
    }
}
