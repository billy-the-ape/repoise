use std::env;
use std::io::{self, Write};
use std::process::ExitCode;

const HELP: &str = "Repoise — offline-first repository knowledge for coding agents

Usage: repoise [--help | --version]

Options:
  -h, --help     Show this help
  -V, --version  Show the version

This initial scaffold does not index repositories yet.
";

fn run() -> io::Result<ExitCode> {
    let args: Vec<_> = env::args_os().skip(1).collect();
    let output = match args.as_slice() {
        [] => format!(
            "Hello from {}! Indexing is not implemented yet.\n",
            repoise_core::NAME
        ),
        [arg] if arg == "--help" || arg == "-h" => HELP.to_owned(),
        [arg] if arg == "--version" || arg == "-V" => {
            format!("repoise {}\n", env!("CARGO_PKG_VERSION"))
        }
        _ => {
            writeln!(
                io::stderr().lock(),
                "error: unsupported arguments; use repoise --help"
            )?;
            return Ok(ExitCode::from(2));
        }
    };
    io::stdout().lock().write_all(output.as_bytes())?;
    Ok(ExitCode::SUCCESS)
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(io::stderr().lock(), "error: {error}");
            ExitCode::FAILURE
        }
    }
}
