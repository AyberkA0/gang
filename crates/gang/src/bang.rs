//! `gang bang`: quick questions for the conflicts a merge left
//! (DESIGN.md §15).

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::io::{self, BufRead, Write};

use anyhow::{Context, Result, bail};
use clap::Parser;
use gang_core::config::Config;
use gang_core::invocation::Invocation;
use gang_core::repo::{Repo, split_nul};
use gang_core::store::{self, Answer, Kind, Pending, Question};
use gang_core::{git, semantic};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::crossterm::terminal;

use crate::ui;

/// Answer quick questions about merge conflicts
#[derive(Parser, Default)]
#[command(name = "gang bang")]
pub struct Args {
    /// Only this file
    path: Option<String>,
    /// List the conflicts without asking
    #[arg(long)]
    list: bool,
    /// Answer the next open question only, without a terminal: mine,
    /// theirs, original, both, delete, keep, reparent or value=<text>.
    /// Every question is answered on its own; there is no way to answer
    /// them all at once.
    #[arg(long, value_name = "CHOICE")]
    answer: Option<String>,
    /// Forget the answers given so far
    #[arg(long)]
    reset: bool,
}

pub fn run(invocation: &Invocation) -> Result<i32> {
    let args = Args::parse_from(
        std::iter::once(OsString::from("gang bang")).chain(invocation.args.iter().cloned()),
    );
    let repo = Repo::discover(&invocation.git_globals)?.context("not inside a git repository")?;
    resolve(&repo, &args)
}

/// Files git still lists as unmerged.
pub fn unmerged(repo: &Repo) -> Result<Vec<String>> {
    Ok(split_nul(&repo.run([
        "diff",
        "--name-only",
        "--diff-filter=U",
        "-z",
    ])?))
}

/// Questions not answered yet, across all files.
pub fn open_questions(repo: &Repo) -> Result<usize> {
    let unmerged = unmerged(repo)?;
    let mut count = 0;
    for pending in store::pending(repo)? {
        if unmerged.contains(&pending.path) {
            count += pending
                .questions
                .iter()
                .filter(|q| !pending.answers.contains_key(&q.id))
                .count();
        }
    }
    let stored: Vec<String> = store::pending(repo)?.into_iter().map(|p| p.path).collect();
    count += unmerged.iter().filter(|p| !stored.contains(p)).count();
    Ok(count)
}

pub fn resolve(repo: &Repo, args: &Args) -> Result<i32> {
    let unmerged = unmerged(repo)?;
    // Forget stored conflicts of files that are no longer unmerged.
    for pending in store::pending(repo)? {
        if !unmerged.contains(&pending.path) {
            store::clear(repo, &pending.path)?;
        }
    }
    let mut files: Vec<Pending> = store::pending(repo)?
        .into_iter()
        .filter(|p| args.path.as_ref().is_none_or(|only| &p.path == only))
        .collect();
    let others: Vec<String> = unmerged
        .iter()
        .filter(|p| !files.iter().any(|f| &f.path == *p))
        .filter(|p| args.path.as_ref().is_none_or(|only| only == *p))
        .cloned()
        .collect();
    if files.is_empty() && others.is_empty() {
        println!("No conflicts to resolve.");
        // Answers applied, but the merge (or rebase…) never finished.
        return if args.list {
            Ok(0)
        } else {
            finish_operation(repo)
        };
    }
    let mut rerere = load_rerere(repo);
    if args.reset {
        // Forget this merge's answers and the remembered ones for the same
        // questions, or they would come straight back (field test A9).
        for file in &mut files {
            file.answers.clear();
            for question in &file.questions {
                rerere.remove(&rerere_key(&file.path, question));
            }
            store::save(repo, file)?;
        }
        save_rerere(repo, &rerere);
    }

    if args.list || (!ui::interactive() && args.answer.is_none()) {
        list(&files, &others);
        if !args.list {
            eprintln!(
                "Run `gang bang` in a terminal to answer them one by one, or answer the next one with `gang bang --answer <choice>`."
            );
            return Ok(1);
        }
        return Ok(0);
    }

    let rules = semantic::rules(&Config::load(repo.root()).unwrap_or_default());
    let total: usize = files.iter().map(|f| f.questions.len()).sum::<usize>() + others.len();
    let mut asked = 0;

    for file in &mut files {
        for question in file.questions.clone() {
            asked += 1;
            if file.answers.contains_key(&question.id) {
                continue;
            }
            let remembered = rerere.get(&rerere_key(&file.path, &question)).cloned();
            let step = match args.answer.as_deref() {
                Some(choice) => {
                    show(file, &question, asked, total, remembered.as_ref());
                    let answer = parse_answer(choice, &question)?;
                    let label = describe_answer(&answer, &question);
                    Step::Answer(answer, label)
                }
                None => ask(file, &question, asked, total, remembered.as_ref())?,
            };
            match step {
                Step::Answer(answer, label) => {
                    println!("> {label}");
                    rerere.insert(rerere_key(&file.path, &question), answer.clone());
                    file.answers.insert(question.id.clone(), answer);
                }
                Step::Undo => {
                    if let Some(last) = file
                        .questions
                        .iter()
                        .rev()
                        .find(|q| file.answers.contains_key(&q.id))
                        .map(|q| q.id.clone())
                    {
                        file.answers.remove(&last);
                    }
                    store::save(repo, file)?;
                    return resolve(repo, args);
                }
                Step::Quit => {
                    store::save(repo, file)?;
                    save_rerere(repo, &rerere);
                    println!("\nYour answers are kept. Run `gang bang` to continue.");
                    return Ok(1);
                }
            }
            store::save(repo, file)?;
            if args.answer.is_some() {
                // One answer per run: every question is seen and answered
                // on its own.
                save_rerere(repo, &rerere);
                let left = total - asked;
                if left > 0 {
                    println!(
                        "{left} question{} left. Run `gang bang` again for the next one.",
                        if left == 1 { "" } else { "s" }
                    );
                    return Ok(1);
                }
            }
        }
    }
    save_rerere(repo, &rerere);

    for path in &others {
        asked += 1;
        match resolve_plain_file(repo, path, args.answer.as_deref(), asked, total)? {
            Plain::Resolved => {
                let left = total - asked;
                if args.answer.is_some() && left > 0 {
                    println!(
                        "{left} question{} left. Run `gang bang` again for the next one.",
                        if left == 1 { "" } else { "s" }
                    );
                    return Ok(1);
                }
            }
            Plain::Partly => {
                println!(
                    "More conflicts are left in {path}. Run `gang bang` again for the next one."
                );
                return Ok(1);
            }
            Plain::Stopped => {
                println!("\nRun `gang bang` to continue.");
                return Ok(1);
            }
        }
    }

    if !files.is_empty() {
        println!();
        println!("Your answers:");
        for file in &files {
            for question in &file.questions {
                if let Some(answer) = file.answers.get(&question.id) {
                    println!(
                        "  {} · {}: {}",
                        short(&file.path),
                        question.place,
                        describe_answer(answer, question)
                    );
                }
            }
        }
        if ui::interactive() && args.answer.is_none() && !ui::confirm("Apply them?", true)? {
            println!("Nothing written. Your answers are kept; run `gang bang` to change them.");
            return Ok(1);
        }
        for file in &files {
            let text = store::apply(repo, file, &rules)?;
            std::fs::write(repo.root().join(&file.path), text)?;
            repo.run(["add", "--", &file.path])?;
            store::clear(repo, &file.path)?;
        }
    }
    ui::done("All conflicts resolved.");
    finish_operation(repo)
}

fn list(files: &[Pending], others: &[String]) {
    for file in files {
        for q in &file.questions {
            let state = if file.answers.contains_key(&q.id) {
                "answered"
            } else {
                "open"
            };
            println!("{}  {}  ({state})", file.path, q.place);
            println!("    {}", q.why);
        }
    }
    for path in others {
        println!("{path}  (text or binary conflict)");
    }
}

fn short(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

enum Step {
    /// The answer and the words of the option that was picked.
    Answer(Answer, String),
    Undo,
    Quit,
}

/// How far a text or binary file got.
enum Plain {
    Resolved,
    /// `--answer` settled one hunk; more are left in the file.
    Partly,
    Stopped,
}

/// Turns `--answer`'s word into an answer that fits the question.
fn parse_answer(choice: &str, q: &Question) -> Result<Answer> {
    let deletion = matches!(q.kind, Kind::DeletedByYou | Kind::DeletedByThem);
    let answer = match (choice, deletion) {
        ("delete", true) => Answer::Delete,
        ("keep", true) => Answer::Keep,
        ("reparent", true) if !q.orphans.is_empty() => Answer::Reparent,
        ("mine", false) => Answer::Ours,
        ("theirs", false) => Answer::Theirs,
        ("original", false) if q.base.is_some() && q.kind == Kind::Value => Answer::Base,
        ("both", false) if both(q).is_some() => Answer::Value(both(q).unwrap_or_default().1),
        (other, false) if q.kind == Kind::Value && other.starts_with("value=") => {
            Answer::Value(other["value=".len()..].to_owned())
        }
        _ => {
            let fits = if deletion {
                if q.orphans.is_empty() {
                    "delete or keep"
                } else {
                    "delete, keep or reparent"
                }
            } else if q.kind == Kind::Value {
                "mine, theirs, original, both or value=<text>"
            } else {
                "mine or theirs"
            };
            bail!("\"{choice}\" does not fit {}: use {fits}", q.place)
        }
    };
    Ok(answer)
}

/// Prints the question and returns its options.
fn show(
    file: &Pending,
    q: &Question,
    n: usize,
    total: usize,
    remembered: Option<&Answer>,
) -> Vec<(char, String, Answer)> {
    let theirs = file.theirs_name.as_str();
    println!();
    println!(
        "{}",
        ui::bold(format!("{} · conflict {n}/{total}", short(&file.path)))
    );
    println!("{}", "─".repeat(58));
    println!("{}", q.place);
    println!();
    let mut options: Vec<(char, String, Answer)> = Vec::new();
    match q.kind {
        Kind::DeletedByYou | Kind::DeletedByThem => {
            println!("Why: {}", q.why);
            println!();
            let orphans = if q.orphans.is_empty() {
                String::new()
            } else {
                format!(" ({} is deleted too)", q.orphans.join(", "))
            };
            options.push(('d', format!("delete{orphans}"), Answer::Delete));
            options.push((
                'k',
                "keep it, with the other side's changes".to_owned(),
                Answer::Keep,
            ));
            if !q.orphans.is_empty() {
                let parent = q
                    .parent
                    .clone()
                    .unwrap_or_else(|| "the scene root".to_owned());
                options.push((
                    'r',
                    format!("keep only {}, move it to {parent}", q.orphans.join(", ")),
                    Answer::Reparent,
                ));
            }
        }
        _ => {
            let width = theirs.len().max(4);
            println!(
                "  {:<width$}  {}",
                "base",
                q.base.as_deref().unwrap_or("(none)")
            );
            println!(
                "  {:<width$}  {}",
                "you",
                q.ours.as_deref().unwrap_or("(deleted)")
            );
            println!(
                "  {:<width$}  {}",
                theirs,
                q.theirs.as_deref().unwrap_or("(deleted)")
            );
            println!();
            println!("Why: {}", q.why);
            println!();
            options.push(('m', "mine".to_owned(), Answer::Ours));
            options.push(('t', "theirs".to_owned(), Answer::Theirs));
            if let Some((text, value)) = both(q) {
                options.push(('b', format!("both ({text})"), Answer::Value(value)));
            }
            if q.base.is_some() && q.kind == Kind::Value {
                options.push(('o', "original".to_owned(), Answer::Base));
            }
            if q.kind == Kind::Value {
                options.push(('e', "edit".to_owned(), Answer::Value(String::new())));
            }
        }
    }
    let line: Vec<String> = options
        .iter()
        .map(|(k, text, _)| format!("[{k}] {text}"))
        .collect();
    println!("{}", line.join("   "));
    if let Some(answer) = remembered {
        println!(
            "Last time you answered this same conflict: {}. [Enter] answer the same again",
            describe_answer(answer, q)
        );
    }
    println!("[?] details   [u] undo   [q] quit");
    options
}

fn ask(
    file: &Pending,
    q: &Question,
    n: usize,
    total: usize,
    remembered: Option<&Answer>,
) -> Result<Step> {
    let options = show(file, q, n, total, remembered);
    loop {
        let key = read_key()?;
        match key {
            'q' => return Ok(Step::Quit),
            'u' => return Ok(Step::Undo),
            '\n' => {
                if let Some(answer) = remembered {
                    let label = format!("{} (as last time)", describe_answer(answer, q));
                    return Ok(Step::Answer(answer.clone(), label));
                }
                continue;
            }
            '?' => {
                println!("\n{}", details(file, q));
                continue;
            }
            'e' if q.kind == Kind::Value => {
                let value = read_line("New value: ")?;
                if value.is_empty() {
                    continue;
                }
                let label = format!("set to {value}");
                return Ok(Step::Answer(Answer::Value(value), label));
            }
            _ => {}
        }
        if let Some((_, text, answer)) = options.iter().find(|(k, _, _)| *k == key) {
            return Ok(Step::Answer(answer.clone(), text.clone()));
        }
    }
}

fn details(file: &Pending, q: &Question) -> String {
    format!(
        "File: {}\nConflict: {}\nBase: {}\nYours: {}\n{}: {}",
        file.path,
        q.id,
        q.base.as_deref().unwrap_or("(none)"),
        q.ours.as_deref().unwrap_or("(none)"),
        file.theirs_name,
        q.theirs.as_deref().unwrap_or("(none)")
    )
}

/// "Both": the sum of both changes, for numbers and vectors.
/// Returns the text shown and the value written.
fn both(q: &Question) -> Option<(String, String)> {
    let (b, o, t) = (q.base.as_deref()?, q.ours.as_deref()?, q.theirs.as_deref()?);
    if let (Ok(b), Ok(o), Ok(t)) = (
        b.trim().parse::<f32>(),
        o.trim().parse::<f32>(),
        t.trim().parse::<f32>(),
    ) {
        let sum = b + (o - b) + (t - b);
        let text = format!("{b} {} {} = {sum}", signed(o - b), signed(t - b));
        return Some((text, sum.to_string()));
    }
    let fields = |s: &str| unity_yaml::Scalar::raw(s).flow_entries();
    let (bf, of, tf) = (fields(b)?, fields(o)?, fields(t)?);
    if bf.iter().any(|(k, _)| k == "fileID") {
        return None;
    }
    let mut out = Vec::new();
    for (key, bv) in &bf {
        let get = |list: &[(String, String)]| {
            list.iter()
                .find(|(k, _)| k == key)
                .and_then(|(_, v)| v.parse::<f32>().ok())
        };
        let (b, o, t) = (bv.parse::<f32>().ok()?, get(&of)?, get(&tf)?);
        out.push((key.clone(), (b + (o - b) + (t - b)).to_string()));
    }
    let value = unity_yaml::Scalar::flow(&out).as_raw().to_owned();
    let shown: Vec<&str> = out.iter().map(|(_, v)| v.as_str()).collect();
    Some((format!("({})", shown.join(", ")), value))
}

fn signed(x: f32) -> String {
    if x < 0.0 {
        format!("− {}", -x)
    } else {
        format!("+ {x}")
    }
}

fn describe_answer(answer: &Answer, q: &Question) -> String {
    match answer {
        Answer::Ours => format!("mine ({})", q.ours.as_deref().unwrap_or("deleted")),
        Answer::Theirs => format!("theirs ({})", q.theirs.as_deref().unwrap_or("deleted")),
        Answer::Base => format!("original ({})", q.base.as_deref().unwrap_or("none")),
        Answer::Value(v) => format!("set to {v}"),
        Answer::Delete => "delete".to_owned(),
        Answer::Keep => "keep".to_owned(),
        Answer::Reparent => "move to the parent".to_owned(),
    }
}

/// One key press, without Enter.
fn read_key() -> Result<char> {
    terminal::enable_raw_mode()?;
    let key = loop {
        match event::read() {
            Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                    break 'q';
                }
                match key.code {
                    KeyCode::Char(c) => break c,
                    KeyCode::Enter => break '\n',
                    KeyCode::Esc => break 'q',
                    _ => {}
                }
            }
            Ok(_) => {}
            Err(err) => {
                terminal::disable_raw_mode()?;
                return Err(err.into());
            }
        }
    };
    terminal::disable_raw_mode()?;
    Ok(key)
}

fn read_line(prompt: &str) -> Result<String> {
    print!("{prompt}");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim().to_owned())
}

/// A text or binary file with a conflict: hunk by hunk, or whole file.
/// With `--answer`, only the next hunk (or the binary file) is answered.
fn resolve_plain_file(
    repo: &Repo,
    path: &str,
    answer: Option<&str>,
    n: usize,
    total: usize,
) -> Result<Plain> {
    let full = repo.root().join(path);
    let bytes = std::fs::read(&full).unwrap_or_default();
    let text = String::from_utf8(bytes)
        .ok()
        .filter(|t| t.contains("<<<<<<< "));
    let Some(text) = text else {
        return resolve_binary(repo, path, answer, n, total);
    };
    let rebasing = repo.rebasing();
    let mut hunks = parse_hunks(&text);
    if rebasing {
        // During a rebase the first side of a hunk is the upstream commit.
        for part in &mut hunks {
            if let Part::Hunk { ours, theirs } = part {
                std::mem::swap(ours, theirs);
            }
        }
    }
    let count = hunks
        .iter()
        .filter(|p| matches!(p, Part::Hunk { .. }))
        .count();
    let mut out = String::new();
    let mut number = 0;
    let mut answered = false;
    for part in &hunks {
        match part {
            Part::Text(t) => out.push_str(t),
            Part::Hunk { ours, theirs } => {
                number += 1;
                if answered {
                    // `--answer` settles one hunk per run; the rest keep
                    // their markers for the next run.
                    out.push_str(&markers(ours, theirs, rebasing));
                    continue;
                }
                println!();
                println!(
                    "{}",
                    ui::bold(format!(
                        "{} · conflict {n}/{total} · hunk {number}/{count}",
                        short(path)
                    ))
                );
                println!("{}", "─".repeat(58));
                println!("you:\n{}", indent(ours));
                println!("theirs:\n{}", indent(theirs));
                println!(
                    "[m] mine   [t] theirs   [b] both (mine first)   [B] both (theirs first)   [e] edit   [q] quit"
                );
                let key = match answer {
                    Some("mine") => 'm',
                    Some("theirs") => 't',
                    Some("both") => 'b',
                    Some("both-theirs-first") => 'B',
                    Some(other) => bail!(
                        "\"{other}\" does not fit a text conflict: use mine, theirs, both or both-theirs-first"
                    ),
                    None => loop {
                        match read_key()? {
                            'q' => return Ok(Plain::Stopped),
                            c @ ('m' | 't' | 'b' | 'B' | 'e') => break c,
                            _ => {}
                        }
                    },
                };
                match key {
                    'm' => out.push_str(ours),
                    't' => out.push_str(theirs),
                    'b' => {
                        out.push_str(ours);
                        out.push_str(theirs);
                    }
                    'B' => {
                        out.push_str(theirs);
                        out.push_str(ours);
                    }
                    _ => out.push_str(&edit(&format!("{ours}{theirs}"))?),
                }
                if answer.is_some() {
                    answered = true;
                }
            }
        }
    }
    std::fs::write(&full, out)?;
    if answered && number < count {
        return Ok(Plain::Partly);
    }
    repo.run(["add", "--", path])?;
    Ok(Plain::Resolved)
}

/// A hunk written back with git's markers, in git's own order.
fn markers(ours: &str, theirs: &str, rebasing: bool) -> String {
    let (first, second) = if rebasing {
        (theirs, ours)
    } else {
        (ours, theirs)
    };
    format!("<<<<<<< ours\n{first}=======\n{second}>>>>>>> theirs\n")
}

fn resolve_binary(
    repo: &Repo,
    path: &str,
    answer: Option<&str>,
    n: usize,
    total: usize,
) -> Result<Plain> {
    // git's --ours / stage 2 is the upstream commit during a rebase.
    let (mine, theirs, theirs_stage) = if repo.rebasing() {
        ("--theirs", "--ours", 2)
    } else {
        ("--ours", "--theirs", 3)
    };
    println!();
    println!(
        "{}",
        ui::bold(format!("{} · conflict {n}/{total}", short(path)))
    );
    println!("{}", "─".repeat(58));
    println!("{path} cannot be merged: you and the other side both changed it.");
    println!("[m] mine   [t] theirs   [a] save theirs next to it to compare   [q] quit");
    let side = match answer {
        Some("mine") => mine,
        Some("theirs") => theirs,
        Some(other) => bail!("\"{other}\" does not fit a binary file: use mine or theirs"),
        None => loop {
            match read_key()? {
                'q' => return Ok(Plain::Stopped),
                'm' => break mine,
                't' => break theirs,
                'a' => {
                    let copy = format!("{path}.theirs");
                    let bytes = repo
                        .output(["show", &format!(":{theirs_stage}:{path}")])?
                        .stdout;
                    std::fs::write(repo.root().join(&copy), bytes)?;
                    println!("Saved theirs as {copy}. Compare, then choose.");
                }
                _ => {}
            }
        },
    };
    println!("> {}", if side == mine { "mine" } else { "theirs" });
    repo.run(["checkout", side, "--", path])?;
    repo.run(["add", "--", path])?;
    Ok(Plain::Resolved)
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|l| format!("  │ {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn edit(text: &str) -> Result<String> {
    let dir = std::env::temp_dir().join(format!("gang-edit-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let file = dir.join("hunk.txt");
    std::fs::write(&file, text)?;
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| {
            if cfg!(windows) {
                "notepad".into()
            } else {
                "vi".into()
            }
        });
    let status = std::process::Command::new(&editor)
        .arg(&file)
        .status()
        .with_context(|| format!("could not start {editor}"))?;
    if !status.success() {
        bail!("{editor} failed");
    }
    let result = std::fs::read_to_string(&file)?;
    let _ = std::fs::remove_dir_all(dir);
    Ok(result)
}

enum Part {
    Text(String),
    Hunk { ours: String, theirs: String },
}

/// Splits a file with git's conflict markers into text and hunks (merge and
/// diff3 styles).
fn parse_hunks(text: &str) -> Vec<Part> {
    let mut parts = Vec::new();
    let mut plain = String::new();
    let mut lines = text.split_inclusive('\n').peekable();
    while let Some(line) = lines.next() {
        if !line.starts_with("<<<<<<< ") {
            plain.push_str(line);
            continue;
        }
        if !plain.is_empty() {
            parts.push(Part::Text(std::mem::take(&mut plain)));
        }
        let (mut ours, mut theirs) = (String::new(), String::new());
        let mut section = 0; // 0 ours, 1 base, 2 theirs
        for line in lines.by_ref() {
            if line.starts_with("||||||| ") || line.trim_end() == "|||||||" {
                section = 1;
            } else if line.trim_end() == "=======" {
                section = 2;
            } else if line.starts_with(">>>>>>> ") || line.trim_end() == ">>>>>>>" {
                break;
            } else if section == 0 {
                ours.push_str(line);
            } else if section == 2 {
                theirs.push_str(line);
            }
        }
        parts.push(Part::Hunk { ours, theirs });
    }
    if !plain.is_empty() {
        parts.push(Part::Text(plain));
    }
    parts
}

/// After the last conflict: offer to finish the merge, or continue the
/// rebase, cherry-pick or revert.
fn finish_operation(repo: &Repo) -> Result<i32> {
    if !unmerged(repo)?.is_empty() {
        return Ok(1);
    }
    let git_dir = repo.git_dir();
    let (what, args): (&str, Vec<&str>) =
        if git_dir.join("rebase-merge").exists() || git_dir.join("rebase-apply").exists() {
            (
                "Continue the rebase",
                vec!["-c", "core.editor=true", "rebase", "--continue"],
            )
        } else if git_dir.join("CHERRY_PICK_HEAD").exists() {
            (
                "Finish the cherry-pick",
                vec!["-c", "core.editor=true", "cherry-pick", "--continue"],
            )
        } else if git_dir.join("REVERT_HEAD").exists() {
            (
                "Finish the revert",
                vec!["-c", "core.editor=true", "revert", "--continue"],
            )
        } else if git_dir.join("MERGE_HEAD").exists() {
            ("Commit the merge", vec!["commit", "--no-edit", "-q"])
        } else {
            return Ok(0);
        };
    if !ui::interactive() || ui::confirm(&format!("{what} now?"), true)? {
        let merging = what == "Commit the merge";
        let code = git::run_inherited(args)?;
        if code == 0 {
            ui::done("Done.");
            if merging {
                if let Err(err) = crate::prefabs_cmd::after_merge(repo, "HEAD^1") {
                    ui::warn(format!(
                        "could not check references after the merge: {err:#}"
                    ));
                }
            }
        }
        return Ok(code);
    }
    Ok(0)
}

fn rerere_key(path: &str, q: &Question) -> String {
    format!(
        "{path}\u{1}{}\u{1}{:?}\u{1}{:?}\u{1}{:?}",
        q.id, q.base, q.ours, q.theirs
    )
}

fn load_rerere(repo: &Repo) -> HashMap<String, Answer> {
    std::fs::read_to_string(repo.gang_dir().join("rerere.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<BTreeMap<String, Answer>>(&t).ok())
        .map(|m| m.into_iter().collect())
        .unwrap_or_default()
}

fn save_rerere(repo: &Repo, answers: &HashMap<String, Answer>) {
    let ordered: BTreeMap<&String, &Answer> = answers.iter().collect();
    if let Ok(text) = serde_json::to_string(&ordered) {
        let _ = std::fs::create_dir_all(repo.gang_dir());
        let _ = std::fs::write(repo.gang_dir().join("rerere.json"), text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(base: &str, ours: &str, theirs: &str) -> Question {
        Question {
            id: "1:x".into(),
            kind: Kind::Value,
            place: String::new(),
            why: String::new(),
            base: Some(base.into()),
            ours: Some(ours.into()),
            theirs: Some(theirs.into()),
            orphans: vec![],
            parent: None,
        }
    }

    #[test]
    fn both_adds_the_changes() {
        assert_eq!(
            both(&q("0", "2", "5")).unwrap(),
            ("0 + 2 + 5 = 7".to_owned(), "7".to_owned())
        );
        assert_eq!(both(&q("1", "1.2", "0.8")).unwrap().1, "1");
        assert_eq!(
            both(&q(
                "{x: 0, y: 1, z: 0}",
                "{x: 2, y: 1, z: 0}",
                "{x: 0, y: 1, z: 3}"
            ))
            .unwrap()
            .1,
            "{x: 2, y: 1, z: 3}"
        );
        assert!(both(&q("Player", "Hero", "Boss")).is_none());
        assert!(both(&q("{fileID: 0}", "{fileID: 1}", "{fileID: 2}")).is_none());
    }

    #[test]
    fn hunks() {
        let text = "a\n<<<<<<< HEAD\nmine\n||||||| base\nold\n=======\ntheirs\n>>>>>>> branch\nz\n";
        let parts = parse_hunks(text);
        assert_eq!(parts.len(), 3);
        match &parts[1] {
            Part::Hunk { ours, theirs } => {
                assert_eq!((ours.as_str(), theirs.as_str()), ("mine\n", "theirs\n"))
            }
            _ => panic!(),
        }
    }
}
