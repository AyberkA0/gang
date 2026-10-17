//! Terminal helpers shared by commands.

use std::io::{self, BufRead, IsTerminal, Write};

/// Whether GANG may ask questions: both stdin and stdout are terminals.
pub fn interactive() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

/// Asks a yes/no question. Enter picks `default`. Never call it when
/// [`interactive`] is false.
pub fn confirm(question: &str, default: bool) -> io::Result<bool> {
    let hint = if default { "[Y/n]" } else { "[y/N]" };
    print!("{question} {hint} ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().lock().read_line(&mut answer)?;
    Ok(match answer.trim().to_ascii_lowercase().as_str() {
        "" => default,
        "y" | "yes" => true,
        _ => false,
    })
}

pub fn done(message: impl AsRef<str>) {
    println!("✓ {}", message.as_ref());
}

pub fn warn(message: impl AsRef<str>) {
    eprintln!("⚠ {}", message.as_ref());
}

pub fn hint(message: impl AsRef<str>) {
    eprintln!("  {}", message.as_ref());
}
