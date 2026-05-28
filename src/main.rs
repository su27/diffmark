mod app;
mod clipboard;
mod diff_parser;
mod git_diff;
mod payload;

use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    name = "vdiff",
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

    /// Disable OSC52 terminal clipboard fallback.
    #[arg(long)]
    no_osc52: bool,
}

fn main() {
    let code = match run() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("vdiff: {err:#}");
            1
        }
    };
    std::process::exit(code);
}

fn run() -> anyhow::Result<i32> {
    let args = Args::parse();

    if args.refresh <= 0.0 {
        eprintln!("vdiff: --refresh must be > 0");
        return Ok(2);
    }
    if args.max_copy_lines == 0 {
        eprintln!("vdiff: --max-copy-lines must be > 0");
        return Ok(2);
    }
    if !git_diff::is_git_repo() {
        eprintln!("vdiff: not inside a git work tree");
        return Ok(2);
    }

    app::run_tui(app::AppConfig {
        context: args.context,
        refresh_interval: std::time::Duration::from_secs_f64(args.refresh),
        max_copy_lines: args.max_copy_lines,
        allow_osc52: !args.no_osc52,
    })?;
    Ok(0)
}
