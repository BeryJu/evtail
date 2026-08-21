//! evtail — `tail(1)` for the Windows Event Log.

mod cli;
mod event;
mod evt;
mod output;
mod query;

use std::io::{self, BufWriter, IsTerminal, Write};
use std::process::ExitCode;

use clap::Parser;
use eyre::{Result, bail};
use windows::Win32::System::Console::{
    CONSOLE_MODE, ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetStdHandle,
    STD_OUTPUT_HANDLE, SetConsoleMode,
};

use crate::cli::{Cli, ColorChoice, Format};
use crate::evt::channels;
use crate::evt::follow::{self, MAX_FOLLOWED};
use crate::evt::reader::{self, Target};
use crate::evt::render::Renderer;
use crate::output::Printer;

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        // `evtail -n 1000 System | head` closes the pipe on us; that is not an
        // error, it is how tail gets used.
        Err(e) if is_broken_pipe(&e) => ExitCode::SUCCESS,
        Err(e) => {
            let mut msg = e.to_string();
            for cause in e.chain().skip(1) {
                msg.push_str(&format!(": {cause}"));
            }
            eprintln!("evtail: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    if cli.list_channels {
        let mut out = BufWriter::new(io::stdout().lock());
        for name in channels::list()? {
            writeln!(out, "{name}")?;
        }
        return out.flush().map_err(Into::into);
    }

    let targets: Vec<Target> = cli
        .target_names()
        .iter()
        .map(|n| Target::parse(n))
        .collect();
    if cli.follow() {
        // Check before printing anything, rather than failing after the
        // historical read has already scrolled past.
        if let Some(file) = targets.iter().find(|t| t.is_file) {
            bail!(
                "--follow needs a channel; `{}` is a file, which cannot grow",
                file.name
            );
        }
        if targets.len() > MAX_FOLLOWED {
            bail!("--follow can watch at most {MAX_FOLLOWED} channels at once");
        }
    }

    let filters = cli.filters();
    let xpath = filters.to_xpath();
    let min_record = filters.client_side_min_record();

    let color = match cli.color {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => io::stdout().is_terminal(),
    };
    if color {
        enable_virtual_terminal();
    }

    let mut renderer = Renderer::new(cli.format() == Format::Xml)?;
    let mut printer = Printer::new(
        BufWriter::new(io::stdout().lock()),
        cli.format(),
        color,
        cli.utc,
        cli.show_headers(targets.len()),
    )
    .flush_each(cli.follow());

    // Read what is already there, remembering where each channel ended.
    let mut bookmarks = Vec::with_capacity(targets.len());
    for target in &targets {
        let history = reader::read(
            target,
            xpath.as_deref(),
            cli.lines,
            min_record,
            cli.retry(),
            &mut renderer,
        )?;
        for ev in &history.events {
            printer.print(&target.name, ev)?;
        }
        bookmarks.push(history.bookmark);
    }
    printer.flush()?;

    if !cli.follow() {
        return Ok(());
    }

    let stop = follow::install_stop_handler()?;
    let mut subs = Vec::with_capacity(targets.len());
    for (target, bookmark) in targets.iter().zip(&bookmarks) {
        subs.push(follow::subscribe(
            target,
            xpath.as_deref(),
            bookmark.as_ref(),
            cli.retry(),
        )?);
    }

    follow::run(&subs, &stop, &mut renderer, |target, ev| {
        printer.print(&target.name, ev)?;
        Ok(())
    })?;
    printer.flush()?;
    Ok(())
}

/// Ask the console to interpret ANSI escapes. Harmless when output is a pipe.
fn enable_virtual_terminal() {
    unsafe {
        let Ok(handle) = GetStdHandle(STD_OUTPUT_HANDLE) else {
            return;
        };
        let mut mode = CONSOLE_MODE(0);
        if GetConsoleMode(handle, &mut mode).is_ok() {
            let _ = SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
        }
    }
}

fn is_broken_pipe(e: &eyre::Report) -> bool {
    e.chain().any(|cause| {
        cause
            .downcast_ref::<io::Error>()
            .is_some_and(|io| io.kind() == io::ErrorKind::BrokenPipe)
    })
}
