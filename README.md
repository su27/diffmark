# vdiff

`vdiff` is a terminal UI for reviewing uncommitted git changes and turning a diff line into an inline review comment that is ready to paste into a command-line AI agent.

It continuously shows `git diff HEAD`, so staged and unstaged tracked changes are both included.

## Install

```sh
cargo install --path .
```

During development:

```sh
cargo run --release -- --context 5 --refresh 1.0 --max-copy-lines 11
```

## Usage

```sh
vdiff
```

Useful options:

```sh
vdiff --context 5 --refresh 1.0 --max-copy-lines 11
```

## Controls

- `j` / `k`: move the selected diff line
- `Shift-j` / `Shift-k`: extend the selection across multiple diff lines
- Mouse wheel: scroll
- Mouse click on a diff line: start a comment for that line
- Mouse drag across diff lines: select multiple lines; release to comment
- `Enter`: start a comment for the selected line or selected line range
- `g` / `G`: jump to top / bottom
- `r`: refresh immediately
- `q`: quit

Comment editor:

- `Enter`: insert a new line
- `Ctrl-D`: submit, copy the review payload, and show the inline comment
- `Esc`: cancel
- Arrow keys, Backspace, Delete, Home, End: edit text

## Clipboard behavior

`vdiff` tries clipboard targets in this order:

1. WSL/Windows clipboard through `powershell.exe` or `clip.exe`
2. Linux/macOS tools such as `wl-copy`, `xclip`, `xsel`, `pbcopy`
3. OSC52 terminal clipboard escape sequence, including tmux passthrough
4. A fallback file at `/tmp/vdiff-last-comment.txt`

## Copied payload

The copied text includes:

- file path and target line number or line range
- the multi-line comment
- a compact hunk snippet around the selected line or line range
- a `>> target:` marker before the first selected diff line
- a `>> comment:` line placed immediately after the selected diff line or selected block

Untracked files are not included yet because `git diff HEAD` does not include them by default.
