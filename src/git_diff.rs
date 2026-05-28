use std::process::Command;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitDiffResult {
    pub text: String,
    pub error: Option<String>,
}

pub fn is_git_repo() -> bool {
    Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

pub fn read_uncommitted_diff(context: usize) -> GitDiffResult {
    if has_head() {
        return run_git_diff(&[
            "diff",
            "--no-color",
            "--no-ext-diff",
            &format!("--unified={context}"),
            "HEAD",
            "--",
        ]);
    }

    let cached = run_git_diff(&[
        "diff",
        "--no-color",
        "--no-ext-diff",
        &format!("--unified={context}"),
        "--cached",
        "--",
    ]);
    let unstaged = run_git_diff(&[
        "diff",
        "--no-color",
        "--no-ext-diff",
        &format!("--unified={context}"),
        "--",
    ]);

    GitDiffResult {
        text: [cached.text, unstaged.text]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        error: cached.error.or(unstaged.error),
    }
}

fn has_head() -> bool {
    Command::new("git")
        .args(["rev-parse", "--verify", "HEAD"])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn run_git_diff(args: &[&str]) -> GitDiffResult {
    let output = match Command::new("git").args(args).output() {
        Ok(output) => output,
        Err(err) => {
            return GitDiffResult {
                text: String::new(),
                error: Some(err.to_string()),
            };
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if output.status.success() || output.status.code() == Some(1) {
        GitDiffResult {
            text: stdout,
            error: None,
        }
    } else {
        GitDiffResult {
            text: stdout,
            error: Some(if stderr.is_empty() {
                "git diff failed".to_string()
            } else {
                stderr
            }),
        }
    }
}
