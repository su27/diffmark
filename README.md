# diffmark

`diffmark` is a terminal UI for reviewing uncommitted Git changes and turning selected diff lines into inline comments you can paste into a command-line AI agent.

It watches staged and unstaged tracked changes, and shows untracked files by default.

Repository: <https://github.com/su27/diffmark>

## Install

```sh
cargo install diffmark
```

`diffmark` requires Rust 1.88 or newer.

For local development:

```sh
cargo install --path .
cargo run --release -- --context 5 --refresh 1.0 --max-copy-lines 11
```

## Usage

```sh
diffmark
```

Common options:

```sh
diffmark --context 5
diffmark --refresh 1.0
diffmark --max-copy-lines 11
diffmark --no-untracked
diffmark --max-untracked-bytes 1048576
diffmark -- src/app.rs README.md
```

Notes:

- Untracked files are shown by default; use `--no-untracked` to hide them.
- Untracked files larger than 1 MiB are skipped by default; use `--max-untracked-bytes` to change the limit.
- Path arguments after `--` limit the diff to those files or directories.

## Controls

- `j` / `k`: move the selection; in visual mode, extend the selected range
- `V`: enter or leave visual-line mode
- `Enter`: comment on the selected line/range, or edit the selected comment row
- `f` / `b`: page forward/backward
- `g` / `G`: jump to top/bottom
- `r`: refresh now
- `Esc`: leave visual mode or cancel comment editing
- `q`: quit

Mouse controls:

- Wheel: scroll
- Click a diff line: comment on that line
- Drag across diff lines: select a range, then comment on it
- Click a comment row, then press `Enter`: edit that comment

## Comment editor

The editor appears inline under the selected diff line or block.

- `Enter`: submit the comment, copy the payload, and show the inline comment
- `Ctrl-J`: insert a new line
- `Esc`: cancel
- Arrow keys, Backspace, Delete, Home, End: edit text

## Clipboard

When a comment is submitted, `diffmark` copies a review payload using the first available method:

1. WSL/Windows clipboard: `clip.exe` or `powershell.exe`
2. Linux/macOS tools: `wl-copy`, `xclip`, `xsel`, `pbcopy`, or `termux-clipboard-set`
3. OSC52 terminal clipboard escape sequence, including tmux passthrough
4. Fallback file: `/tmp/diffmark-last-comment.txt`

Use `--no-osc52` to disable the OSC52 fallback.

## Copied payload

The copied text includes:

- file path and target line number or line range
- your comment
- a compact diff snippet around the selected line/range
- `>> target:` for single-line selections
- `>> target start` / `>> target end` for multi-line selections
- `>> comment:` immediately after the selected line/block

## Privacy

`diffmark` does not send data over the network. It only runs `git`, reads local files when rendering untracked changes, and calls local clipboard tools.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo package --locked
```
