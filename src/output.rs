use std::{
    env,
    io::{self, IsTerminal, Write},
};

use anyhow::{Context, Result};
use serde_json::Value;

#[derive(Clone, Copy)]
pub struct Output {
    json: bool,
}

impl Output {
    pub fn new(json: bool) -> Self {
        Self { json }
    }

    pub fn is_json(self) -> bool {
        self.json
    }

    pub fn line(self, value: &str) -> Result<()> {
        let stdout = io::stdout();
        let mut stdout = stdout.lock();
        writeln!(stdout, "{value}").context("could not write terminal output")?;
        stdout.flush().context("could not flush terminal output")
    }

    pub fn fragment(self, value: &str) -> Result<()> {
        let stdout = io::stdout();
        let mut stdout = stdout.lock();
        write!(stdout, "{value}").context("could not write terminal output")?;
        stdout.flush().context("could not flush terminal output")
    }

    pub fn event(self, value: &Value) -> Result<()> {
        self.events(std::slice::from_ref(value))
    }

    pub fn events(self, values: &[Value]) -> Result<()> {
        let stdout = io::stdout();
        let mut stdout = stdout.lock();
        for value in values {
            serde_json::to_writer(&mut stdout, value)?;
            writeln!(stdout).context("could not write terminal output")?;
        }
        stdout.flush().context("could not flush terminal output")
    }

    pub fn status(self, label: &str, value: &str) {
        if self.json {
            return;
        }
        eprintln!("{} {value}", style(label, "1;36"));
    }

    pub fn instruction(self, value: &str) {
        if !self.json {
            eprintln!("{}", style(value, "2"));
        }
    }

    pub fn blank_line(self) {
        if !self.json {
            eprintln!();
        }
    }
}

fn style(value: &str, code: &str) -> String {
    if io::stderr().is_terminal() && env::var_os("NO_COLOR").is_none() {
        format!("\x1b[{code}m{value}\x1b[0m")
    } else {
        value.to_owned()
    }
}
