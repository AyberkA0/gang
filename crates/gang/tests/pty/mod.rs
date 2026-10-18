//! A pseudo terminal for the interactive screens: GANG only asks questions
//! when stdin and stdout are terminals (ConPTY on Windows).
#![allow(dead_code)]

use std::io::{Read, Write};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

const TIMEOUT: Duration = Duration::from_secs(60);

pub struct Pty {
    master: Option<Box<dyn MasterPty + Send>>,
    writer: Option<Box<dyn Write + Send>>,
    child: Box<dyn Child + Send + Sync>,
    output: Receiver<Vec<u8>>,
    raw: Vec<u8>,
    /// How much of the screen text `expect` has already matched.
    seen: usize,
    closed: bool,
}

impl Pty {
    /// Runs `command` (program, arguments, folder and environment) in a
    /// fresh terminal.
    pub fn spawn(command: &Command) -> Pty {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 50,
                cols: 200,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut builder = CommandBuilder::new(command.get_program());
        builder.args(command.get_args());
        if let Some(dir) = command.get_current_dir() {
            builder.cwd(dir);
        }
        for (key, value) in command.get_envs() {
            match value {
                Some(value) => builder.env(key, value),
                None => builder.env_remove(key),
            }
        }
        builder.env("TERM", "xterm-256color");
        let child = pair.slave.spawn_command(builder).unwrap();
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().unwrap();
        let writer = pair.master.take_writer().unwrap();
        let (sender, output) = mpsc::channel();
        thread::spawn(move || {
            let mut buffer = [0u8; 4096];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if sender.send(buffer[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Pty {
            master: Some(pair.master),
            writer: Some(writer),
            child,
            output,
            raw: Vec::new(),
            seen: 0,
            closed: false,
        }
    }

    /// Everything shown so far, without terminal control sequences.
    pub fn screen(&self) -> String {
        clean(&self.raw)
    }

    /// Waits until `text` appears after what was matched before, and returns
    /// the screen text up to its end.
    pub fn expect(&mut self, text: &str) -> String {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let screen = self.screen();
            if let Some(at) = screen.get(self.seen..).and_then(|rest| rest.find(text)) {
                let end = self.seen + at + text.len();
                let shown = screen[self.seen..end].to_owned();
                self.seen = end;
                return shown;
            }
            if self.closed || Instant::now() > deadline {
                panic!(
                    "did not see {text:?}; after the last match the terminal showed:\n{}",
                    screen.get(self.seen..).unwrap_or(&screen)
                );
            }
            self.pump(Duration::from_millis(100));
        }
    }

    /// Types `text`; "\r" is Enter.
    pub fn send(&mut self, text: &str) {
        let writer = self.writer.as_mut().unwrap();
        writer.write_all(text.as_bytes()).unwrap();
        writer.flush().unwrap();
    }

    /// Waits for the program to exit; returns its exit code and the whole
    /// screen text.
    pub fn finish(mut self) -> (u32, String) {
        let deadline = Instant::now() + TIMEOUT;
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() > deadline {
                let _ = self.child.kill();
                panic!("the program did not exit:\n{}", self.screen());
            }
            self.pump(Duration::from_millis(50));
        };
        // Closing the terminal ends the output stream.
        self.writer = None;
        self.master = None;
        while !self.closed && Instant::now() < deadline {
            self.pump(Duration::from_millis(200));
        }
        (status.exit_code(), self.screen())
    }

    /// Reads what arrived, answering cursor position requests (ConPTY
    /// waits for an answer before it starts the program).
    fn pump(&mut self, wait: Duration) {
        match self.output.recv_timeout(wait) {
            Ok(bytes) => {
                if contains(&bytes, b"\x1b[6n")
                    && let Some(writer) = self.writer.as_mut()
                {
                    let _ = writer.write_all(b"\x1b[1;1R");
                    let _ = writer.flush();
                }
                self.raw.extend(bytes);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => self.closed = true,
        }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        if !self.closed {
            let _ = self.child.kill();
        }
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Terminal output as plain text: control sequences removed, cursor moves
/// turned into the spaces or line breaks they stand for.
pub fn clean(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.next() {
                Some('[') => {
                    let mut params = String::new();
                    let mut last = None;
                    for c in chars.by_ref() {
                        if ('\x40'..='\x7e').contains(&c) {
                            last = Some(c);
                            break;
                        }
                        params.push(c);
                    }
                    match last {
                        Some('C') => {
                            let n = params.parse().unwrap_or(1);
                            out.extend(std::iter::repeat_n(' ', n));
                        }
                        Some('H') if !out.ends_with('\n') && !out.is_empty() => out.push('\n'),
                        _ => {}
                    }
                }
                Some(']') => {
                    // Operating system command, up to BEL or ESC \.
                    while let Some(c) = chars.next() {
                        if c == '\x07' {
                            break;
                        }
                        if c == '\x1b' {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\r' => {}
            '\x07' | '\x08' => {}
            c => out.push(c),
        }
    }
    out
}
