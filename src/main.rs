mod app;
mod clipboard;
mod diff_parser;
mod git_diff;
mod payload;

use clap::Parser;
use git_diff::DiffOptions;

const DEFAULT_MAX_UNTRACKED_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Parser)]
#[command(
    name = "diffmark",
    version,
    about = "Review uncommitted git diffs in a TUI and copy inline comments for command-line AI agents."
)]
struct Args {
    /// Git diff unified context lines to request.
    #[arg(long, default_value_t = 5)]
    context: usize,

    /// Seconds between automatic diff refreshes.
    #[arg(long, default_value_t = 1.0)]
    refresh: f64,

    /// Maximum diff lines copied around the selected line.
    #[arg(long, default_value_t = 11)]
    max_copy_lines: usize,

    /// Hide untracked files.
    #[arg(long)]
    no_untracked: bool,

    /// Maximum size of each untracked file to render.
    #[arg(long, default_value_t = DEFAULT_MAX_UNTRACKED_BYTES)]
    max_untracked_bytes: u64,

    /// Disable OSC52 terminal clipboard fallback.
    #[arg(long)]
    no_osc52: bool,

    /// Limit the diff to these paths.
    #[arg(value_name = "PATH", trailing_var_arg = true)]
    pathspecs: Vec<String>,
}

fn main() {
    let code = match run() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("diffmark: {err:#}");
            1
        }
    };
    std::process::exit(code);
}

fn run() -> anyhow::Result<i32> {
    let args = Args::parse();

    if args.refresh <= 0.0 {
        eprintln!("diffmark: --refresh must be > 0");
        return Ok(2);
    }
    if args.max_copy_lines == 0 {
        eprintln!("diffmark: --max-copy-lines must be > 0");
        return Ok(2);
    }
    if !git_diff::is_git_repo() {
        eprintln!("diffmark: not inside a git work tree");
        return Ok(2);
    }

    app::run_tui(app::AppConfig {
        diff: DiffOptions {
            context: args.context,
            include_untracked: !args.no_untracked,
            max_untracked_bytes: args.max_untracked_bytes,
            pathspecs: args.pathspecs,
        },
        refresh_interval: std::time::Duration::from_secs_f64(args.refresh),
        max_copy_lines: args.max_copy_lines,
        allow_osc52: !args.no_osc52,
    })?;
    Ok(0)
}
