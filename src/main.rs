mod cli;
mod color;
mod commands;
mod core;
mod exit;
mod style_spec;
mod term;
mod theme;

mod ui;

use clap::Parser;

use crate::cli::{Cli, Command};
use crate::color::ColorProfile;
use crate::exit::{AppError, OK};
use crate::term::tty::UiStream;
use crate::theme::Palette;
use crate::ui::loop_::UiMode;

fn main() {
    // Die quietly on a closed pipe (`rat ... | head`) like other unix
    // tools, instead of panicking on EPIPE.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    // Windows has no SIGPIPE: a closed pipe surfaces as a println! panic.
    // Exit quietly instead of spraying a backtrace. (Kept on unix too as a
    // belt for writes that race the signal disposition.)
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info
            .payload()
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| info.payload().downcast_ref::<&str>().copied())
            .unwrap_or("");
        // Windows closed-pipe errors: 109 ERROR_BROKEN_PIPE ("the pipe has
        // been ended"), 232 ERROR_NO_DATA ("the pipe is being closed").
        if payload.contains("Broken pipe")
            || payload.contains("os error 109")
            || payload.contains("os error 232")
        {
            std::process::exit(0);
        }
        default_hook(info);
    }));
    std::process::exit(real_main());
}

fn real_main() -> i32 {
    let cli = Cli::parse();
    // Capability comes from the UI stream's ttyness, never stdout's, so
    // command substitution keeps color.
    let is_tty = UiStream::open().is_tty();
    let profile = color::resolve_profile(cli.color, &color::SystemEnv, is_tty);
    // Resolved once, here, and threaded by value; nothing downstream
    // asks the environment a second time.
    let mode = ui::loop_::resolve_ui_mode(cli.no_accessible, cli.accessible);
    // Asked at most once per process, here, before any command runs and long
    // before anything claims raw mode. Commands never ask.
    let detected = if !speaks_instead_of_painting(&cli.command, mode)
        && theme::may_detect(cli.appearance, profile)
    {
        term::appearance::probe(theme::PROBE_TIMEOUT)
            .map(|a| (a, theme::AppearanceSource::Osc))
            .or_else(|| {
                theme::appearance_from_colorfgbg(&color::SystemEnv)
                    .map(|a| (a, theme::AppearanceSource::ColorFgBg))
            })
    } else {
        None
    };
    let (appearance, source) = theme::resolve_appearance(cli.appearance, detected);
    let palette = theme::Palette::builtin(appearance, source);

    match dispatch(cli.command, profile, palette, mode) {
        Ok(()) => OK,
        Err(err) => {
            match &err {
                AppError::Fail(inner) => eprintln!("rat: {inner:#}"),
                AppError::Timeout(None) => eprintln!("timed out"),
                AppError::Timeout(Some(detail)) => eprintln!("rat: {detail:#}"),
                AppError::NoSelection | AppError::Aborted | AppError::Child(_) => {}
            }
            err.code()
        }
    }
}

/// Whether this process will speak its UI instead of painting it — the
/// only case where the startup appearance query buys nothing, because
/// nothing will read the palette it answers.
///
/// The transcript reaches three commands. Every other command paints
/// exactly as it always did, however the variable is set: a reader who
/// exports it in their shell profile must not find their tables and
/// dashboards quietly re-coloured. The list below and dispatch's arms
/// below IT differ by one: the diagnostic receives the mode so it can
/// report it, and keeps painting.
fn speaks_instead_of_painting(command: &Command, mode: UiMode) -> bool {
    mode == UiMode::Echo
        && matches!(
            command,
            Command::Choose(_) | Command::Confirm(_) | Command::Filter(_)
        )
}

fn dispatch(
    command: Command,
    profile: ColorProfile,
    palette: Palette,
    mode: UiMode,
) -> exit::AppResult {
    match command {
        Command::Style(args) => commands::style::run(args, profile, palette),
        Command::Bar(args) => commands::bar::run(args, profile, palette),
        Command::Table(args) => commands::table::run(args, profile, palette),
        Command::Join(args) => commands::join::run(args, profile, palette),
        Command::Duration(args) => commands::duration::run(args, profile, palette),
        Command::Date(args) => commands::date::run(args, profile, palette),
        Command::Spark(args) => commands::spark::run(args, profile, palette),
        Command::Log(args) => commands::log::run(args, profile, palette),
        Command::Frame(args) => commands::frame::run(args, profile, palette),
        Command::Watch(args) => commands::watch::run(args, profile, palette),
        Command::Dashboard(args) => commands::dashboard::run(args, profile, palette),
        Command::Doctor(args) => commands::doctor::run(args, profile, palette, mode),
        Command::Choose(args) => commands::choose::run(args, profile, palette, mode),
        Command::Confirm(args) => commands::confirm::run(args, profile, palette, mode),
        Command::Input(args) => commands::input::run(args, profile, palette),
        Command::Filter(args) => commands::filter::run(args, profile, palette, mode),
        Command::Spin(args) => commands::spin::run(args, profile, palette),
        Command::Completion(args) => commands::completion::run(args, profile, palette),
        #[cfg(debug_assertions)]
        Command::Env(args) => {
            match std::env::var(&args.name) {
                Ok(value) => println!("{value}"),
                Err(_) => println!("unset"),
            }
            Ok(())
        }
        #[cfg(debug_assertions)]
        Command::ExitCode(args) => {
            if let Some(msg) = &args.stderr_msg {
                eprintln!("{msg}");
            }
            match args.code {
                0 => Ok(()),
                1 => Err(AppError::NoSelection),
                124 => Err(AppError::Timeout(None)),
                130 => Err(AppError::Aborted),
                n => Err(AppError::Child(n)),
            }
        }
        #[cfg(debug_assertions)]
        Command::Sleep(args) => {
            std::thread::sleep(std::time::Duration::from_millis(args.millis));
            if let Some(text) = &args.text {
                println!("{text}");
            }
            Ok(())
        }
        #[cfg(debug_assertions)]
        Command::Cat(args) => {
            use std::io::Write;

            use anyhow::Context as _;
            let bytes = std::fs::read(&args.file).context("reading")?;
            let mut out = std::io::stdout().lock();
            out.write_all(&bytes).context("writing")?;
            out.flush().context("flushing")?;
            Ok(())
        }
        #[cfg(debug_assertions)]
        Command::Lines(args) => {
            use std::io::Write;

            use anyhow::Context as _;
            fn flood(mut out: impl Write, count: usize) -> anyhow::Result<()> {
                for i in 0..count {
                    writeln!(out, "{i}").context("writing")?;
                }
                out.flush().context("flushing")
            }
            if args.stderr {
                flood(
                    std::io::BufWriter::new(std::io::stderr().lock()),
                    args.count,
                )?;
            } else {
                flood(
                    std::io::BufWriter::new(std::io::stdout().lock()),
                    args.count,
                )?;
            }
            Ok(())
        }
        #[cfg(debug_assertions)]
        Command::Follow(args) => {
            use std::io::{Read as _, Seek as _, Write};

            /// Copy whatever has appeared past `offset` and advance it.
            /// Returns false once the sink is gone, which is how this
            /// child learns its reader closed the pipe.
            fn pump(path: &std::path::Path, offset: &mut u64, mut sink: impl Write) -> bool {
                let Ok(mut file) = std::fs::File::open(path) else {
                    // A file that is not there yet is empty, not fatal:
                    // a follower outlives the thing it follows.
                    return true;
                };
                if file.seek(std::io::SeekFrom::Start(*offset)).is_err() {
                    return true;
                }
                let mut buf = Vec::new();
                let Ok(read) = file.read_to_end(&mut buf) else {
                    return true;
                };
                if read == 0 {
                    return true;
                }
                *offset += read as u64;
                // Flushed per pump, never buffered: an unflushed fixture
                // would reproduce the very pipeline-buffering trap these
                // tests must not measure, and the delay would look like
                // rat failing to follow.
                sink.write_all(&buf).is_ok() && sink.flush().is_ok()
            }

            let (mut out_at, mut err_at) = (0u64, 0u64);
            loop {
                if !pump(&args.file, &mut out_at, std::io::stdout().lock()) {
                    break;
                }
                if let Some(path) = &args.stderr_file
                    && !pump(path, &mut err_at, std::io::stderr().lock())
                {
                    break;
                }
                // A poll rather than a filesystem watch: portable, and
                // short enough that a test waiting seconds never notices.
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    fn command_of(args: &[&str]) -> Command {
        let mut argv = vec!["rat"];
        argv.extend_from_slice(args);
        Cli::parse_from(argv).command
    }

    #[test]
    fn only_the_spoken_pickers_skip_the_startup_query() {
        // The three the transcript reaches.
        for args in [
            &["choose", "alpha"][..],
            &["confirm", "Ship it?"],
            &["filter"],
        ] {
            let command = command_of(args);
            assert!(
                speaks_instead_of_painting(&command, UiMode::Echo),
                "{args:?}"
            );
            assert!(
                !speaks_instead_of_painting(&command, UiMode::Painted),
                "{args:?}"
            );
        }
        // The diagnostic RECEIVES the mode — dispatch hands it over so
        // the report can name it — and still paints. It exists to say
        // what this terminal does, so one that stopped asking would
        // report the default palette on a light terminal to the one
        // person who ran it to find out why. This row is why the
        // predicate is not a copy of dispatch's arm list.
        assert!(!speaks_instead_of_painting(
            &command_of(&["doctor"]),
            UiMode::Echo
        ));
        // And every other command paints however the variable is set: a
        // reader who exports it in their profile must not find their
        // tables and dashboards quietly re-coloured.
        for args in [
            &["style", "x"][..],
            &["table"],
            &["watch", "--", "true"],
            &["input"],
            &["spin", "--", "true"],
            &["bar"],
        ] {
            assert!(
                !speaks_instead_of_painting(&command_of(args), UiMode::Echo),
                "{args:?}"
            );
        }
    }
}
