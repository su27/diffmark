use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use base64::{Engine as _, engine::general_purpose};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClipboardResult {
    pub ok: bool,
    pub message: String,
}

pub fn copy_to_clipboard(text: &str, allow_osc52: bool) -> ClipboardResult {
    for method in candidate_methods() {
        if try_method(&method, text).is_ok() {
            return ClipboardResult {
                ok: true,
                message: format!("copied to clipboard via {method}"),
            };
        }
    }

    if allow_osc52 && copy_osc52(text).is_ok() {
        return ClipboardResult {
            ok: true,
            message: "sent OSC52 clipboard sequence; terminal support may vary".to_string(),
        };
    }

    let fallback = env::temp_dir().join("vdiff-last-comment.txt");
    if let Err(err) = fs::write(&fallback, text) {
        return ClipboardResult {
            ok: false,
            message: format!("clipboard unavailable; failed to write fallback: {err}"),
        };
    }

    ClipboardResult {
        ok: false,
        message: format!(
            "clipboard unavailable; payload written to {}",
            fallback.display()
        ),
    }
}

fn candidate_methods() -> Vec<String> {
    let mut methods = Vec::new();
    if is_wsl() {
        if command_exists("powershell.exe") {
            methods.push("powershell.exe".to_string());
        }
        if command_exists("clip.exe") {
            methods.push("clip.exe".to_string());
        }
    }

    for command in ["wl-copy", "xclip", "xsel", "pbcopy", "termux-clipboard-set"] {
        if command_exists(command) {
            methods.push(command.to_string());
        }
    }

    if !is_wsl() {
        if command_exists("powershell.exe") {
            methods.push("powershell.exe".to_string());
        }
        if command_exists("clip.exe") {
            methods.push("clip.exe".to_string());
        }
    }

    methods
}

fn try_method(method: &str, text: &str) -> io::Result<()> {
    match method {
        "powershell.exe" => pipe_to(
            "powershell.exe",
            &[
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "[Console]::InputEncoding = [System.Text.Encoding]::UTF8; $text = [Console]::In.ReadToEnd(); Set-Clipboard -Value $text",
            ],
            text.as_bytes(),
        ),
        "clip.exe" => {
            let mut input = vec![0xff, 0xfe];
            for unit in text.encode_utf16() {
                input.extend_from_slice(&unit.to_le_bytes());
            }
            pipe_to("clip.exe", &[], &input)
        }
        "wl-copy" => pipe_to("wl-copy", &[], text.as_bytes()),
        "xclip" => pipe_to("xclip", &["-selection", "clipboard"], text.as_bytes()),
        "xsel" => pipe_to("xsel", &["--clipboard", "--input"], text.as_bytes()),
        "pbcopy" => pipe_to("pbcopy", &[], text.as_bytes()),
        "termux-clipboard-set" => pipe_to("termux-clipboard-set", &[], text.as_bytes()),
        _ => Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("unknown clipboard method: {method}"),
        )),
    }
}

fn pipe_to(command: &str, args: &[&str], input: &[u8]) -> io::Result<()> {
    let mut child = Command::new(command)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;

    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(input)?;
    }
    drop(child.stdin.take());

    let output = child.wait_with_output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ))
    }
}

fn copy_osc52(text: &str) -> io::Result<()> {
    let encoded = general_purpose::STANDARD.encode(text.as_bytes());
    let mut sequence = format!("\x1b]52;c;{encoded}\x07");
    if env::var_os("TMUX").is_some() {
        sequence = format!("\x1bPtmux;{}\x1b\\", sequence.replace('\x1b', "\x1b\x1b"));
    }
    io::stdout().write_all(sequence.as_bytes())?;
    io::stdout().flush()
}

fn command_exists(command: &str) -> bool {
    if command.contains('/') {
        return PathBuf::from(command).is_file();
    }
    let Some(path) = env::var_os("PATH") else {
        return false;
    };
    env::split_paths(&path).any(|dir| dir.join(command).is_file())
}

fn is_wsl() -> bool {
    fs::read_to_string("/proc/version")
        .map(|value| {
            let lower = value.to_ascii_lowercase();
            lower.contains("microsoft") || lower.contains("wsl")
        })
        .unwrap_or(false)
}
