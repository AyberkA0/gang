//! The team panel: `gang` with no command (DESIGN.md §8.2, §9).
//!
//! While it is open it watches the working tree, publishes this user's
//! presence after changes (debounced) and on the heartbeat, and polls the
//! team's presence.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use gang_core::config::{Config, parse_duration};
use gang_core::presence::{self, Presence, State};
use gang_core::repo::Repo;
use gang_core::{attributes, clock};
use notify::{RecursiveMode, Watcher};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::{DefaultTerminal, Frame};

/// Folders whose changes never matter to the team.
const IGNORED: &[&str] = &[
    ".git",
    "Library",
    "Temp",
    "Logs",
    "obj",
    "UserSettings",
    "Build",
    "Builds",
];

enum Job {
    Publish { force: bool },
    Poll,
}

enum Message {
    FileChanged,
    Finished {
        what: &'static str,
        result: Result<String, String>,
    },
}

struct View {
    title: String,
    me: Presence,
    team: Vec<Presence>,
    synced_at: Option<i64>,
    status: String,
}

pub fn run(repo: Repo) -> Result<i32> {
    let config = Config::load(repo.root())?;
    let debounce = parse_duration(&config.presence.debounce)?;
    let poll_interval = parse_duration(&config.presence.poll_interval)?;
    let heartbeat = parse_duration(&config.presence.heartbeat)?;
    let offline_after = presence::offline_after(&config);

    let (messages_tx, messages) = mpsc::channel::<Message>();
    let jobs = start_worker(repo.root().to_owned(), config.clone(), messages_tx.clone());
    let _watcher = start_watcher(repo.root(), messages_tx)?;

    let mut view = View {
        title: repo
            .root()
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        me: presence::current(&repo, &config, None)?,
        team: Vec::new(),
        synced_at: None,
        status: "Connecting to the team…".to_owned(),
    };
    refresh(&repo, &config, &mut view);
    let _ = jobs.send(Job::Publish { force: true });
    let _ = jobs.send(Job::Poll);

    let mut terminal = ratatui::try_init().context("could not open the panel")?;
    let mut changed_at: Option<Instant> = None;
    let mut last_poll = Instant::now();
    let mut last_heartbeat = Instant::now();

    let result = loop {
        if let Err(err) = terminal.draw(|frame| draw(frame, &view, offline_after)) {
            break Err(err.into());
        }
        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break Ok(0),
                    KeyCode::Char('r') => {
                        view.status = "Refreshing…".to_owned();
                        let _ = jobs.send(Job::Publish { force: true });
                        let _ = jobs.send(Job::Poll);
                    }
                    KeyCode::Char('s') => {
                        terminal = run_outside(terminal, &["save"])?;
                        refresh(&repo, &config, &mut view);
                        let _ = jobs.send(Job::Publish { force: false });
                    }
                    _ => {}
                }
            }
        }

        while let Ok(message) = messages.try_recv() {
            match message {
                Message::FileChanged => changed_at = Some(Instant::now()),
                Message::Finished { what, result } => {
                    match result {
                        Ok(note) => {
                            if what == "poll" {
                                view.synced_at = Some(clock::now());
                            }
                            if !note.is_empty() {
                                view.status = note;
                            }
                        }
                        Err(err) => view.status = format!("⚠ {what} failed: {err}"),
                    }
                    refresh(&repo, &config, &mut view);
                }
            }
        }
        if changed_at.is_some_and(|at| at.elapsed() >= debounce) {
            changed_at = None;
            refresh(&repo, &config, &mut view);
            let _ = jobs.send(Job::Publish { force: false });
        }
        if last_poll.elapsed() >= poll_interval {
            last_poll = Instant::now();
            let _ = jobs.send(Job::Poll);
        }
        if last_heartbeat.elapsed() >= heartbeat {
            last_heartbeat = Instant::now();
            let _ = jobs.send(Job::Publish { force: false });
        }
    };
    ratatui::restore();
    result
}

fn start_worker(root: PathBuf, config: Config, messages: Sender<Message>) -> Sender<Job> {
    let (jobs_tx, jobs) = mpsc::channel::<Job>();
    thread::spawn(move || {
        let Ok(repo) = Repo::open(&root) else {
            return;
        };
        for job in jobs {
            let (what, result) = match job {
                Job::Publish { force } => (
                    "publish",
                    presence::publish(&repo, &config, force).map(|_| String::new()),
                ),
                Job::Poll => ("poll", presence::poll(&repo).map(|_| String::new())),
            };
            let result = result.map_err(|e| format!("{e:#}"));
            if messages.send(Message::Finished { what, result }).is_err() {
                break;
            }
        }
    });
    jobs_tx
}

fn start_watcher(root: &Path, messages: Sender<Message>) -> Result<notify::RecommendedWatcher> {
    let root_owned = root.to_owned();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else { return };
        let relevant = event.paths.iter().any(|path| {
            path.strip_prefix(&root_owned)
                .ok()
                .and_then(|rel| rel.components().next())
                .map(|first| !IGNORED.contains(&first.as_os_str().to_string_lossy().as_ref()))
                .unwrap_or(false)
        });
        if relevant {
            let _ = messages.send(Message::FileChanged);
        }
    })?;
    watcher.watch(root, RecursiveMode::Recursive)?;
    Ok(watcher)
}

fn refresh(repo: &Repo, config: &Config, view: &mut View) {
    if let Ok(me) = presence::current(repo, config, None) {
        let login = me.user.clone();
        view.me = me;
        if let Ok(team) = presence::team(repo) {
            view.team = team.into_iter().filter(|p| p.user != login).collect();
        }
    }
}

/// Leaves the panel, runs `gang <args>` in the terminal, and comes back.
fn run_outside(terminal: DefaultTerminal, args: &[&str]) -> Result<DefaultTerminal> {
    drop(terminal);
    ratatui::restore();
    let exe = std::env::current_exe()?;
    let _ = std::process::Command::new(exe).args(args).status();
    print!("\nPress Enter to return to the panel.");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    ratatui::try_init().context("could not reopen the panel")
}

fn draw(frame: &mut Frame, view: &View, offline_after: i64) {
    let now = clock::now();
    let [team_area, mine_area, status_area] = Layout::vertical([
        Constraint::Length(view.team.len().max(1) as u16 + 3),
        Constraint::Min(4),
        Constraint::Length(2),
    ])
    .areas(frame.area());

    let synced = match view.synced_at {
        Some(at) => format!("synced {}", clock::ago(now - at)),
        None => "connecting…".to_owned(),
    };
    let branch = view.me.branch.clone().unwrap_or_else(|| "-".to_owned());
    let title = format!(" {} · {branch} · {synced} ", view.title);

    let mut team_lines = vec![person_line(&view.me, true, true, now)];
    for person in &view.team {
        let online = person.is_online(now, offline_after);
        team_lines.push(person_line(person, false, online, now));
    }
    frame.render_widget(
        Paragraph::new(team_lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .title_bottom(" TEAM "),
        ),
        team_area,
    );

    let others: BTreeMap<&str, Vec<&str>> = view
        .team
        .iter()
        .flat_map(|p| {
            p.files
                .iter()
                .map(move |(f, flag)| (f.as_str(), p.user.as_str(), flag.state))
        })
        .filter(|(_, _, state)| *state != State::Stashed)
        .fold(BTreeMap::new(), |mut map, (file, user, _)| {
            map.entry(file).or_default().push(user);
            map
        });
    let mut mine_lines: Vec<Line> = view
        .me
        .files
        .iter()
        .map(|(file, flag)| {
            let mut spans = vec![
                Span::styled(
                    format!("  {:<9} ", flag.state.label()),
                    Style::default().fg(Color::Cyan),
                ),
                Span::raw(file.clone()),
            ];
            if let Some(users) = others.get(file.as_str()) {
                let unique: BTreeSet<&&str> = users.iter().collect();
                let names = unique.into_iter().copied().collect::<Vec<_>>().join(", ");
                let (color, text) = if attributes::is_binary_path(file) {
                    (
                        Color::Red,
                        format!("  ✗ also changed by {names}; cannot be merged"),
                    )
                } else {
                    (Color::Yellow, format!("  ⚠ also changed by {names}"))
                };
                spans.push(Span::styled(text, Style::default().fg(color)));
            }
            Line::from(spans)
        })
        .collect();
    if mine_lines.is_empty() {
        mine_lines.push(Line::from("  Nothing changed."));
    }
    frame.render_widget(
        Paragraph::new(mine_lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" YOUR CHANGES "),
        ),
        mine_area,
    );

    let keys = Line::from(vec![
        Span::styled(" q", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" quit  "),
        Span::styled("r", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" refresh  "),
        Span::styled("s", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" save  "),
        Span::styled(
            format!("  {}", view.status),
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    frame.render_widget(Paragraph::new(keys), status_area);
}

fn person_line(person: &Presence, you: bool, online: bool, now: i64) -> Line<'static> {
    let (dot, color) = if online {
        ("●", Color::Green)
    } else {
        ("○", Color::DarkGray)
    };
    let name = if you {
        format!("{} (you)", person.user)
    } else {
        person.user.clone()
    };
    let changed = person
        .files
        .values()
        .filter(|f| f.state != State::Stashed)
        .count();
    let mut summary = match (changed, person.ahead) {
        (0, 0) => "clean".to_owned(),
        (c, 0) => format!("{c} changed"),
        (0, a) => format!("{a} unpushed commit{}", if a == 1 { "" } else { "s" }),
        (c, a) => format!("{c} changed · {a} unpushed"),
    };
    if !online {
        summary = format!("offline {} · {summary}", clock::ago(person.age(now)));
    }
    if person.editor_open {
        summary.push_str(" · Unity open");
    }
    Line::from(vec![
        Span::styled(format!(" {dot} "), Style::default().fg(color)),
        Span::styled(
            format!("{name:<22}"),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            "{:<9}{:<16}",
            person.os,
            person.branch.clone().unwrap_or_default()
        )),
        Span::raw(summary),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use gang_core::presence::FileFlag;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn person(user: &str, ts: i64, files: &[(&str, State)]) -> Presence {
        Presence {
            schema: 1,
            user: user.into(),
            github_id: None,
            seq: 1,
            ts: clock::format(ts),
            machine: "m".into(),
            os: "macos".into(),
            unity: None,
            editor_open: false,
            branch: Some("main".into()),
            head: None,
            ahead: 0,
            files: files
                .iter()
                .map(|(f, s)| {
                    (
                        f.to_string(),
                        FileFlag {
                            state: *s,
                            since: clock::format(ts),
                        },
                    )
                })
                .collect(),
            notices: Vec::new(),
        }
    }

    #[test]
    fn draws_team_and_overlaps() {
        let now = clock::now();
        let view = View {
            title: "forest-run".into(),
            me: person(
                "ayberk",
                now,
                &[
                    ("Assets/Scenes/Level1.unity", State::Modified),
                    ("Assets/Art/Hero.psd", State::Modified),
                ],
            ),
            team: vec![
                person(
                    "mehmet",
                    now,
                    &[("Assets/Scenes/Level1.unity", State::Modified)],
                ),
                person(
                    "zeynep",
                    now - 3600,
                    &[("Assets/Art/Hero.psd", State::Unpushed)],
                ),
            ],
            synced_at: Some(now),
            status: String::new(),
        };
        let mut terminal = Terminal::new(TestBackend::new(110, 16)).unwrap();
        terminal.draw(|frame| draw(frame, &view, 300)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("forest-run · main · synced just now"));
        assert!(text.contains("ayberk (you)"));
        assert!(text.contains("offline 1 h ago"));
        assert!(text.contains("⚠ also changed by mehmet"));
        assert!(text.contains("✗ also changed by zeynep; cannot be merged"));
    }
}
