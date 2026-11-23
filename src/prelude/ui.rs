//! How Prelude asks: a terminal (passwords typed hidden), the same over any input (scripted tests), or not at all
//! (`--yes`: every question takes its default, which the flags and the current settings fill in).

use std::io::{BufRead, Write};

pub trait Ui {
    fn say(&mut self, text: &str);
    /// A line; Enter takes the default.
    fn ask(&mut self, q: &str, default: &str) -> String;
    /// A secret, typed hidden; Enter keeps `keep` (the current one) when there is one.
    fn secret(&mut self, q: &str, keep: Option<&str>) -> String;
    fn yes(&mut self, q: &str, default: bool) -> bool;
    /// One of the options (their number); Enter takes the default.
    fn choose(&mut self, q: &str, options: &[&str], default: usize) -> usize;
    /// Not interactive: `--yes`.
    fn auto(&self) -> bool {
        false
    }
    /// No answers left (the input ended): a question asked again until it's answered would be asked forever.
    fn ended(&mut self) -> bool {
        false
    }
}

/// Questions and answers over a reader and a writer (the terminal, or a script in tests).
pub struct Term<R: BufRead, W: Write> {
    pub input: R,
    pub out: W,
    /// typed secrets are hidden (a real terminal)
    pub hide: bool,
}

impl<R: BufRead, W: Write> Term<R, W> {
    fn line(&mut self) -> String {
        let mut s = String::new();
        let _ = self.input.read_line(&mut s);
        s.trim_end_matches(['\n', '\r']).to_string()
    }
}

impl<R: BufRead, W: Write> Ui for Term<R, W> {
    fn say(&mut self, text: &str) {
        let _ = writeln!(self.out, "{text}");
    }
    fn ask(&mut self, q: &str, default: &str) -> String {
        if default.is_empty() {
            let _ = write!(self.out, "{q}: ");
        } else {
            let _ = write!(self.out, "{q} [{default}]: ");
        }
        let _ = self.out.flush();
        let a = self.line();
        if a.trim().is_empty() { default.to_string() } else { a.trim().to_string() }
    }
    fn secret(&mut self, q: &str, keep: Option<&str>) -> String {
        let note = if keep.is_some_and(|k| !k.is_empty()) { " (Enter keeps the current one)" } else { "" };
        let _ = write!(self.out, "{q}{note}: ");
        let _ = self.out.flush();
        let a = if self.hide { rpassword::read_password().unwrap_or_default() } else { self.line() };
        if a.is_empty() { keep.unwrap_or("").to_string() } else { a }
    }
    fn yes(&mut self, q: &str, default: bool) -> bool {
        let _ = write!(self.out, "{q} [{}]: ", if default { "Y/n" } else { "y/N" });
        let _ = self.out.flush();
        match self.line().trim().to_lowercase().as_str() {
            "" => default,
            a => a.starts_with('y'),
        }
    }
    fn choose(&mut self, q: &str, options: &[&str], default: usize) -> usize {
        self.say(q);
        for (i, o) in options.iter().enumerate() {
            self.say(&format!("  {}. {o}", i + 1));
        }
        loop {
            let a = self.ask("Your choice", &(default + 1).to_string());
            if let Ok(n) = a.parse::<usize>()
                && (1..=options.len()).contains(&n)
            {
                return n - 1;
            }
            self.say(&format!("Type a number from 1 to {}.", options.len()));
            if self.input.fill_buf().map(|b| b.is_empty()).unwrap_or(true) {
                return default; // the script ended
            }
        }
    }
    fn ended(&mut self) -> bool {
        // a real terminal (secrets typed hidden) doesn't end; only a script, a pipe or /dev/null does
        !self.hide && self.input.fill_buf().map(|b| b.is_empty()).unwrap_or(true)
    }
}

/// `--yes`: no questions; the defaults (from the flags and the current settings) are the answers.
pub struct Auto<W: Write> {
    pub out: W,
}

impl<W: Write> Ui for Auto<W> {
    fn say(&mut self, text: &str) {
        let _ = writeln!(self.out, "{text}");
    }
    fn ask(&mut self, _q: &str, default: &str) -> String {
        default.to_string()
    }
    fn secret(&mut self, _q: &str, keep: Option<&str>) -> String {
        keep.unwrap_or("").to_string()
    }
    fn yes(&mut self, _q: &str, default: bool) -> bool {
        default
    }
    fn choose(&mut self, _q: &str, _o: &[&str], default: usize) -> usize {
        default
    }
    fn auto(&self) -> bool {
        true
    }
}
