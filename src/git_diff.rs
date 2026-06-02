use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::str;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitDiffResult {
    pub text: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiffOptions {
    pub context: usize,
    pub include_untracked: bool,
    pub max_untracked_bytes: u64,
    pub pathspecs: Vec<String>,
}

pub fn is_git_repo() -> bool {
    git_output(["rev-parse", "--is-inside-work-tree"], None)
        .map(|output| output.status.success())
        .unwrap_or(false)
}

pub fn read_uncommitted_diff(options: &DiffOptions) -> GitDiffResult {
    read_uncommitted_diff_in(options, None)
}

fn read_uncommitted_diff_in(options: &DiffOptions, cwd: Option<&Path>) -> GitDiffResult {
    let tracked = read_tracked_diff(options, cwd);
    if options.include_untracked {
        combine_diff_results([tracked, read_untracked_diff(options, cwd)])
    } else {
        tracked
    }
}

fn read_tracked_diff(options: &DiffOptions, cwd: Option<&Path>) -> GitDiffResult {
    if has_head(cwd) {
        return run_git_diff(
            diff_args(options.context, ["HEAD"], &options.pathspecs),
            cwd,
        );
    }

    let cached = run_git_diff(
        diff_args(options.context, ["--cached"], &options.pathspecs),
        cwd,
    );
    let unstaged = run_git_diff(diff_args(options.context, [], &options.pathspecs), cwd);

    combine_diff_results([cached, unstaged])
}

fn diff_args<'a>(
    context: usize,
    extra: impl IntoIterator<Item = &'a str>,
    pathspecs: &[String],
) -> Vec<OsString> {
    let mut args: Vec<OsString> = ["diff", "--no-color", "--no-ext-diff"]
        .into_iter()
        .map(OsString::from)
        .collect();
    args.push(OsString::from(format!("--unified={context}")));
    args.extend(extra.into_iter().map(OsString::from));
    args.push(OsString::from("--"));
    args.extend(pathspecs.iter().map(OsString::from));
    args
}

fn read_untracked_diff(options: &DiffOptions, cwd: Option<&Path>) -> GitDiffResult {
    let paths = match list_untracked_files(&options.pathspecs, cwd) {
        Ok(paths) => paths,
        Err(error) => {
            return GitDiffResult {
                text: String::new(),
                error: Some(error),
            };
        }
    };

    combine_diff_results(
        paths
            .into_iter()
            .map(|path| synthesize_untracked_diff(&path, cwd, options.max_untracked_bytes)),
    )
}

fn list_untracked_files(pathspecs: &[String], cwd: Option<&Path>) -> Result<Vec<String>, String> {
    let mut args: Vec<OsString> = ["ls-files", "--others", "--exclude-standard", "-z", "--"]
        .into_iter()
        .map(OsString::from)
        .collect();
    args.extend(pathspecs.iter().map(OsString::from));

    let output = git_output(args, cwd).map_err(|err| err.to_string())?;
    if output.status.success() {
        return Ok(split_nul_paths(&output.stdout));
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(if stderr.is_empty() {
        "git ls-files failed".to_string()
    } else {
        stderr
    })
}

fn synthesize_untracked_diff(
    path: &str,
    cwd: Option<&Path>,
    max_untracked_bytes: u64,
) -> GitDiffResult {
    let disk_path = path_in_cwd(path, cwd);
    let metadata = match fs::symlink_metadata(&disk_path) {
        Ok(metadata) => metadata,
        Err(err) => return diff_error(format!("failed to read untracked file {path}: {err}")),
    };
    if metadata.file_type().is_symlink() {
        let target = match fs::read_link(&disk_path) {
            Ok(target) => target.to_string_lossy().into_owned(),
            Err(err) => {
                return diff_error(format!("failed to read untracked symlink {path}: {err}"));
            }
        };
        return GitDiffResult {
            text: text_untracked_diff(path, "120000", &target),
            error: None,
        };
    }

    if !metadata.is_file() {
        return GitDiffResult {
            text: skipped_untracked_diff(path, "100644", "not a regular file"),
            error: None,
        };
    }
    if metadata.len() > max_untracked_bytes {
        return GitDiffResult {
            text: skipped_untracked_diff(
                path,
                file_mode(&metadata),
                &format!(
                    "larger than --max-untracked-bytes ({})",
                    max_untracked_bytes
                ),
            ),
            error: None,
        };
    }

    let bytes = match fs::read(&disk_path) {
        Ok(bytes) => bytes,
        Err(err) => return diff_error(format!("failed to read untracked file {path}: {err}")),
    };
    if bytes.is_empty() {
        return GitDiffResult {
            text: untracked_header(path, file_mode(&metadata)),
            error: None,
        };
    }
    if bytes.contains(&0) {
        return GitDiffResult {
            text: binary_untracked_diff(path, file_mode(&metadata)),
            error: None,
        };
    }

    let text = match str::from_utf8(&bytes) {
        Ok(text) => text,
        Err(_) => {
            return GitDiffResult {
                text: binary_untracked_diff(path, file_mode(&metadata)),
                error: None,
            };
        }
    };

    GitDiffResult {
        text: text_untracked_diff(path, file_mode(&metadata), text),
        error: None,
    }
}

fn diff_error(error: String) -> GitDiffResult {
    GitDiffResult {
        text: String::new(),
        error: Some(error),
    }
}

fn path_in_cwd(path: &str, cwd: Option<&Path>) -> PathBuf {
    cwd.map_or_else(|| PathBuf::from(path), |cwd| cwd.join(path))
}

fn untracked_header(path: &str, mode: &'static str) -> String {
    format!(
        "diff --git {} {}\nnew file mode {mode}\n--- /dev/null\n+++ {}\n",
        quote_prefixed_path("a/", path),
        quote_prefixed_path("b/", path),
        quote_prefixed_path("b/", path)
    )
}

fn text_untracked_diff(path: &str, mode: &'static str, text: &str) -> String {
    let lines: Vec<&str> = text.split_terminator('\n').collect();
    let missing_final_newline = !text.ends_with('\n');
    let mut output = untracked_header(path, mode);
    output.push_str(&format!("@@ -0,0 +1,{} @@\n", lines.len()));
    for line in lines {
        output.push('+');
        output.push_str(line);
        output.push('\n');
    }
    if missing_final_newline {
        output.push_str("\\ No newline at end of file\n");
    }
    output
}

fn binary_untracked_diff(path: &str, mode: &'static str) -> String {
    let mut output = untracked_header(path, mode);
    output.push_str(&format!(
        "Binary files /dev/null and {} differ\n",
        quote_prefixed_path("b/", path)
    ));
    output
}

fn skipped_untracked_diff(path: &str, mode: &'static str, reason: &str) -> String {
    let mut output = untracked_header(path, mode);
    output.push_str(&format!("diffmark: skipped untracked file ({reason})\n"));
    output
}

fn file_mode(_metadata: &fs::Metadata) -> &'static str {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if _metadata.permissions().mode() & 0o111 != 0 {
            return "100755";
        }
    }
    "100644"
}

fn quote_prefixed_path(prefix: &str, path: &str) -> String {
    quote_path(&format!("{prefix}{path}"))
}

fn quote_path(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|ch| !ch.is_whitespace() && ch != '"' && ch != '\\')
    {
        return value.to_string();
    }

    let mut output = String::from("\"");
    for ch in value.chars() {
        match ch {
            '\\' => output.push_str("\\\\"),
            '"' => output.push_str("\\\""),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            other => output.push(other),
        }
    }
    output.push('"');
    output
}

fn split_nul_paths(output: &[u8]) -> Vec<String> {
    output
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect()
}

fn has_head(cwd: Option<&Path>) -> bool {
    git_output(["rev-parse", "--verify", "HEAD"], cwd)
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn run_git_diff<I, S>(args: I, cwd: Option<&Path>) -> GitDiffResult
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = match git_output(args, cwd) {
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

fn git_output<I, S>(args: I, cwd: Option<&Path>) -> std::io::Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new("git");
    command.args(args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    command.output()
}

fn combine_diff_results(results: impl IntoIterator<Item = GitDiffResult>) -> GitDiffResult {
    let mut parts = Vec::new();
    let mut errors = Vec::new();

    for result in results {
        if !result.text.is_empty() {
            parts.push(result.text);
        }
        if let Some(error) = result.error {
            errors.push(error);
        }
    }

    GitDiffResult {
        text: parts.join("\n"),
        error: if errors.is_empty() {
            None
        } else {
            Some(errors.join("; "))
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff_parser::parse_unified_diff;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn split_nul_paths_keeps_spaces_and_drops_empty_tail() {
        assert_eq!(
            split_nul_paths(b"one.txt\0path with space.txt\0"),
            vec!["one.txt", "path with space.txt"]
        );
    }

    #[test]
    fn read_diff_includes_untracked_files() {
        let repo = temp_repo();
        init_repo(&repo);
        fs::write(repo.join("tracked.txt"), "old\n").unwrap();
        run_git_checked(["add", "tracked.txt"], &repo);
        run_git_checked(["commit", "-m", "initial"], &repo);

        fs::write(repo.join("tracked.txt"), "new\n").unwrap();
        fs::write(repo.join("untracked.txt"), "hello\nworld\n").unwrap();

        let result = read_uncommitted_diff_in(&test_options(true, &[]), Some(&repo));

        fs::remove_dir_all(&repo).unwrap();
        assert_eq!(result.error, None);
        assert!(
            result
                .text
                .contains("diff --git a/tracked.txt b/tracked.txt")
        );
        assert!(
            result
                .text
                .contains("diff --git a/untracked.txt b/untracked.txt"),
            "{}",
            result.text
        );
        assert!(result.text.contains("+hello"));
    }

    #[test]
    fn read_diff_can_skip_untracked_files() {
        let repo = temp_repo();
        init_repo(&repo);
        fs::write(repo.join("tracked.txt"), "old\n").unwrap();
        run_git_checked(["add", "tracked.txt"], &repo);
        run_git_checked(["commit", "-m", "initial"], &repo);

        fs::write(repo.join("tracked.txt"), "new\n").unwrap();
        fs::write(repo.join("untracked.txt"), "hello\n").unwrap();

        let result = read_uncommitted_diff_in(&test_options(false, &[]), Some(&repo));

        fs::remove_dir_all(&repo).unwrap();
        assert_eq!(result.error, None);
        assert!(
            result
                .text
                .contains("diff --git a/tracked.txt b/tracked.txt")
        );
        assert!(!result.text.contains("untracked.txt"), "{}", result.text);
    }

    #[test]
    fn pathspec_filters_tracked_and_untracked_files() {
        let repo = temp_repo();
        init_repo(&repo);
        fs::create_dir(repo.join("src")).unwrap();
        fs::write(repo.join("src/tracked.txt"), "old\n").unwrap();
        fs::write(repo.join("other.txt"), "old\n").unwrap();
        run_git_checked(["add", "."], &repo);
        run_git_checked(["commit", "-m", "initial"], &repo);

        fs::write(repo.join("src/tracked.txt"), "new\n").unwrap();
        fs::write(repo.join("other.txt"), "new\n").unwrap();
        fs::write(repo.join("src/untracked.txt"), "hello\n").unwrap();
        fs::write(repo.join("untracked-root.txt"), "hello\n").unwrap();

        let result = read_uncommitted_diff_in(&test_options(true, &["src"]), Some(&repo));

        fs::remove_dir_all(&repo).unwrap();
        assert!(result.text.contains("src/tracked.txt"), "{}", result.text);
        assert!(result.text.contains("src/untracked.txt"), "{}", result.text);
        assert!(!result.text.contains("other.txt"), "{}", result.text);
        assert!(
            !result.text.contains("untracked-root.txt"),
            "{}",
            result.text
        );
    }

    #[test]
    fn synthesized_untracked_diff_quotes_paths_with_spaces() {
        let result = synthesize_untracked_diff_for_text("path with space.txt", "hello\n");
        let parsed = parse_unified_diff(&result.text);
        let added = parsed
            .lines
            .iter()
            .find(|line| line.raw == "+hello")
            .unwrap();

        assert_eq!(result.error, None);
        assert_eq!(added.file_path.as_deref(), Some("path with space.txt"));
    }

    #[test]
    fn synthesized_untracked_diff_marks_missing_final_newline() {
        let result = synthesize_untracked_diff_for_text("new.txt", "hello");

        assert_eq!(result.error, None);
        assert!(result.text.contains("+hello\n\\ No newline at end of file"));
    }

    #[test]
    fn oversized_untracked_files_are_shown_as_skipped() {
        let repo = temp_repo();
        init_repo(&repo);
        fs::write(repo.join("big.txt"), "hello\n").unwrap();

        let mut options = test_options(true, &[]);
        options.max_untracked_bytes = 1;
        let result = read_uncommitted_diff_in(&options, Some(&repo));

        fs::remove_dir_all(&repo).unwrap();
        assert_eq!(result.error, None);
        assert!(result.text.contains("diff --git a/big.txt b/big.txt"));
        assert!(result.text.contains("skipped untracked file"));
        assert!(!result.text.contains("+hello"));
    }

    #[cfg(unix)]
    #[test]
    fn untracked_symlinks_are_rendered_as_symlink_diffs() {
        use std::os::unix::fs::symlink;

        let repo = temp_repo();
        init_repo(&repo);
        symlink("target.txt", repo.join("link.txt")).unwrap();

        let result = read_uncommitted_diff_in(&test_options(true, &[]), Some(&repo));

        fs::remove_dir_all(&repo).unwrap();
        assert_eq!(result.error, None);
        assert!(result.text.contains("new file mode 120000"));
        assert!(result.text.contains("+target.txt"));
    }

    fn synthesize_untracked_diff_for_text(path: &str, text: &str) -> GitDiffResult {
        let repo = temp_repo();
        init_repo(&repo);
        fs::write(repo.join(path), text).unwrap();
        let result = synthesize_untracked_diff(path, Some(&repo), 1024);
        fs::remove_dir_all(&repo).unwrap();
        result
    }

    fn test_options(include_untracked: bool, pathspecs: &[&str]) -> DiffOptions {
        DiffOptions {
            context: 3,
            include_untracked,
            max_untracked_bytes: 1024 * 1024,
            pathspecs: pathspecs.iter().map(|path| path.to_string()).collect(),
        }
    }

    fn init_repo(repo: &Path) {
        run_git_checked(["init"], repo);
        run_git_checked(["config", "user.email", "diffmark@example.invalid"], repo);
        run_git_checked(["config", "user.name", "diffmark"], repo);
    }

    fn temp_repo() -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("diffmark-test-{}-{nanos}", std::process::id()));
        fs::create_dir(&path).unwrap();
        path
    }

    fn run_git_checked<const N: usize>(args: [&str; N], cwd: &Path) {
        let output = git_output(args, Some(cwd)).unwrap();
        assert!(
            output.status.success(),
            "git command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
