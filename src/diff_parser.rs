#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedDiff {
    pub lines: Vec<DiffLine>,
    pub hunks: Vec<Hunk>,
}

impl ParsedDiff {
    pub fn selectable_count(&self) -> usize {
        self.lines.iter().filter(|line| line.selectable).count()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Hunk {
    pub index: usize,
    pub file_path: Option<String>,
    pub header: String,
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
    pub section: String,
    pub line_indices: Vec<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LineKind {
    File,
    Meta,
    Hunk,
    Add,
    Delete,
    Context,
    Note,
}

impl LineKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Meta => "meta",
            Self::Hunk => "hunk",
            Self::Add => "add",
            Self::Delete => "delete",
            Self::Context => "context",
            Self::Note => "note",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiffLine {
    pub raw: String,
    pub kind: LineKind,
    pub file_path: Option<String>,
    pub old_lineno: Option<usize>,
    pub new_lineno: Option<usize>,
    pub hunk_index: Option<usize>,
    pub hunk_line_index: Option<usize>,
    pub selectable: bool,
}

impl DiffLine {
    pub fn stable_id(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}",
            self.file_path.as_deref().unwrap_or_default(),
            self.hunk_index
                .map(|value| value.to_string())
                .unwrap_or_default(),
            self.kind.as_str(),
            self.old_lineno
                .map(|value| value.to_string())
                .unwrap_or_default(),
            self.new_lineno
                .map(|value| value.to_string())
                .unwrap_or_default(),
            self.raw
        )
    }
}

pub fn parse_unified_diff(text: &str) -> ParsedDiff {
    let mut lines = Vec::new();
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut current_file: Option<String> = None;
    let mut old_file: Option<String> = None;
    let mut current_hunk: Option<usize> = None;
    let mut old_cursor: usize = 0;
    let mut new_cursor: usize = 0;

    for raw in text.lines() {
        if let Some((old_path, new_path)) = parse_diff_git_paths(raw) {
            old_file = old_path;
            current_file = new_path;
            current_hunk = None;
            lines.push(DiffLine {
                raw: raw.to_string(),
                kind: LineKind::File,
                file_path: current_file.clone(),
                old_lineno: None,
                new_lineno: None,
                hunk_index: None,
                hunk_line_index: None,
                selectable: false,
            });
            continue;
        }

        if let Some(path) = raw.strip_prefix("--- ") {
            old_file = clean_header_path(path);
            lines.push(DiffLine {
                raw: raw.to_string(),
                kind: LineKind::Meta,
                file_path: current_file.clone(),
                old_lineno: None,
                new_lineno: None,
                hunk_index: None,
                hunk_line_index: None,
                selectable: false,
            });
            continue;
        }

        if let Some(path) = raw.strip_prefix("+++ ") {
            let next_file = clean_header_path(path);
            current_file = next_file.or_else(|| old_file.clone());
            lines.push(DiffLine {
                raw: raw.to_string(),
                kind: LineKind::Meta,
                file_path: current_file.clone(),
                old_lineno: None,
                new_lineno: None,
                hunk_index: None,
                hunk_line_index: None,
                selectable: false,
            });
            continue;
        }

        if let Some(header) = parse_hunk_header(raw) {
            let index = hunks.len();
            current_hunk = Some(index);
            old_cursor = header.old_start;
            new_cursor = header.new_start;
            hunks.push(Hunk {
                index,
                file_path: current_file.clone(),
                header: raw.to_string(),
                old_start: header.old_start,
                old_count: header.old_count,
                new_start: header.new_start,
                new_count: header.new_count,
                section: header.section,
                line_indices: Vec::new(),
            });
            lines.push(DiffLine {
                raw: raw.to_string(),
                kind: LineKind::Hunk,
                file_path: current_file.clone(),
                old_lineno: None,
                new_lineno: None,
                hunk_index: Some(index),
                hunk_line_index: None,
                selectable: false,
            });
            continue;
        }

        if let Some(hunk_index) = current_hunk
            && let Some((line, next_old, next_new)) = parse_hunk_line(
                raw,
                &hunks[hunk_index],
                old_cursor,
                new_cursor,
                hunks[hunk_index].line_indices.len(),
            )
        {
            old_cursor = next_old;
            new_cursor = next_new;
            lines.push(line);
            let line_index = lines.len() - 1;
            hunks[hunk_index].line_indices.push(line_index);
            continue;
        }

        current_hunk = None;
        lines.push(DiffLine {
            raw: raw.to_string(),
            kind: LineKind::Meta,
            file_path: current_file.clone(),
            old_lineno: None,
            new_lineno: None,
            hunk_index: None,
            hunk_line_index: None,
            selectable: false,
        });
    }

    ParsedDiff { lines, hunks }
}

fn parse_hunk_line(
    raw: &str,
    hunk: &Hunk,
    old_cursor: usize,
    new_cursor: usize,
    hunk_line_index: usize,
) -> Option<(DiffLine, usize, usize)> {
    let first = raw.as_bytes().first().copied()? as char;
    match first {
        '+' => Some((
            DiffLine {
                raw: raw.to_string(),
                kind: LineKind::Add,
                file_path: hunk.file_path.clone(),
                old_lineno: None,
                new_lineno: Some(new_cursor),
                hunk_index: Some(hunk.index),
                hunk_line_index: Some(hunk_line_index),
                selectable: true,
            },
            old_cursor,
            new_cursor.saturating_add(1),
        )),
        '-' => Some((
            DiffLine {
                raw: raw.to_string(),
                kind: LineKind::Delete,
                file_path: hunk.file_path.clone(),
                old_lineno: Some(old_cursor),
                new_lineno: None,
                hunk_index: Some(hunk.index),
                hunk_line_index: Some(hunk_line_index),
                selectable: true,
            },
            old_cursor.saturating_add(1),
            new_cursor,
        )),
        ' ' => Some((
            DiffLine {
                raw: raw.to_string(),
                kind: LineKind::Context,
                file_path: hunk.file_path.clone(),
                old_lineno: Some(old_cursor),
                new_lineno: Some(new_cursor),
                hunk_index: Some(hunk.index),
                hunk_line_index: Some(hunk_line_index),
                selectable: true,
            },
            old_cursor.saturating_add(1),
            new_cursor.saturating_add(1),
        )),
        '\\' => Some((
            DiffLine {
                raw: raw.to_string(),
                kind: LineKind::Note,
                file_path: hunk.file_path.clone(),
                old_lineno: None,
                new_lineno: None,
                hunk_index: Some(hunk.index),
                hunk_line_index: Some(hunk_line_index),
                selectable: false,
            },
            old_cursor,
            new_cursor,
        )),
        _ => None,
    }
}

struct HunkHeader {
    old_start: usize,
    old_count: usize,
    new_start: usize,
    new_count: usize,
    section: String,
}

fn parse_hunk_header(raw: &str) -> Option<HunkHeader> {
    let rest = raw.strip_prefix("@@ ")?;
    let marker = rest.find(" @@")?;
    let range_part = &rest[..marker];
    let section = rest[marker + 3..].trim().to_string();
    let mut pieces = range_part.split_whitespace();
    let old_spec = pieces.next()?;
    let new_spec = pieces.next()?;
    let (old_start, old_count) = parse_range(old_spec, '-')?;
    let (new_start, new_count) = parse_range(new_spec, '+')?;
    Some(HunkHeader {
        old_start,
        old_count,
        new_start,
        new_count,
        section,
    })
}

fn parse_range(spec: &str, prefix: char) -> Option<(usize, usize)> {
    let rest = spec.strip_prefix(prefix)?;
    let mut pieces = rest.splitn(2, ',');
    let start = pieces.next()?.parse().ok()?;
    let count = pieces
        .next()
        .map(|value| value.parse().ok())
        .unwrap_or(Some(1))?;
    Some((start, count))
}

fn clean_header_path(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let path = if trimmed.starts_with('"') {
        split_quoted_tokens(trimmed).into_iter().next()?
    } else {
        trimmed.split('\t').next().unwrap_or(trimmed).to_string()
    };
    clean_diff_path(&path)
}

fn clean_diff_path(value: &str) -> Option<String> {
    if value == "/dev/null" {
        return None;
    }
    let path = value
        .strip_prefix("a/")
        .or_else(|| value.strip_prefix("b/"))
        .unwrap_or(value);
    Some(path.to_string())
}

fn parse_diff_git_paths(raw: &str) -> Option<(Option<String>, Option<String>)> {
    let rest = raw.strip_prefix("diff --git ")?;
    let tokens = split_quoted_tokens(rest);
    if tokens.len() < 2 {
        return None;
    }
    Some((clean_diff_path(&tokens[0]), clean_diff_path(&tokens[1])))
}

fn split_quoted_tokens(value: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = value.chars().collect();
    let mut index = 0;

    while index < chars.len() {
        while index < chars.len() && chars[index].is_whitespace() {
            index += 1;
        }
        if index >= chars.len() {
            break;
        }

        if chars[index] == '"' {
            index += 1;
            let mut token = String::new();
            while index < chars.len() {
                let ch = chars[index];
                index += 1;
                if ch == '"' {
                    break;
                }
                if ch == '\\' && index < chars.len() {
                    let escaped = chars[index];
                    index += 1;
                    match escaped {
                        'n' => token.push('\n'),
                        'r' => token.push('\r'),
                        't' => token.push('\t'),
                        '\\' => token.push('\\'),
                        '"' => token.push('"'),
                        other => token.push(other),
                    }
                } else {
                    token.push(ch);
                }
            }
            tokens.push(token);
        } else {
            let start = index;
            while index < chars.len() && !chars[index].is_whitespace() {
                index += 1;
            }
            tokens.push(chars[start..index].iter().collect());
        }
    }

    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = concat!(
        "diff --git a/src/foo.py b/src/foo.py\n",
        "index 1111111..2222222 100644\n",
        "--- a/src/foo.py\n",
        "+++ b/src/foo.py\n",
        "@@ -1,5 +1,6 @@\n",
        " def main():\n",
        "-    old()\n",
        "+    new()\n",
        "+    added()\n",
        "     keep()\n",
        "\\ No newline at end of file\n",
    );

    #[test]
    fn parses_line_numbers_and_hunks() {
        let parsed = parse_unified_diff(SAMPLE);
        assert_eq!(parsed.hunks.len(), 1);
        assert_eq!(parsed.hunks[0].file_path.as_deref(), Some("src/foo.py"));
        assert_eq!(parsed.selectable_count(), 5);

        let changed: Vec<&DiffLine> = parsed.lines.iter().filter(|line| line.selectable).collect();
        assert_eq!(changed[0].kind, LineKind::Context);
        assert_eq!(
            (changed[0].old_lineno, changed[0].new_lineno),
            (Some(1), Some(1))
        );
        assert_eq!(changed[1].kind, LineKind::Delete);
        assert_eq!(
            (changed[1].old_lineno, changed[1].new_lineno),
            (Some(2), None)
        );
        assert_eq!(changed[2].kind, LineKind::Add);
        assert_eq!(
            (changed[2].old_lineno, changed[2].new_lineno),
            (None, Some(2))
        );
        assert_eq!(changed[3].kind, LineKind::Add);
        assert_eq!(
            (changed[3].old_lineno, changed[3].new_lineno),
            (None, Some(3))
        );
        assert_eq!(changed[4].kind, LineKind::Context);
        assert_eq!(
            (changed[4].old_lineno, changed[4].new_lineno),
            (Some(3), Some(4))
        );
    }

    #[test]
    fn stable_id_contains_anchor_data() {
        let parsed = parse_unified_diff(SAMPLE);
        let added = parsed
            .lines
            .iter()
            .find(|line| line.kind == LineKind::Add)
            .unwrap();
        let id = added.stable_id();
        assert!(id.contains("src/foo.py"));
        assert!(id.contains("add"));
        assert!(id.contains('2'));
        assert!(id.contains("+    new()"));
    }

    #[test]
    fn paths_with_spaces() {
        let parsed = parse_unified_diff(
            "diff --git \"a/path with space.txt\" \"b/path with space.txt\"\n\
--- \"a/path with space.txt\"\n\
+++ \"b/path with space.txt\"\n\
@@ -1 +1 @@\n\
-old\n\
+new\n",
        );
        let added = parsed
            .lines
            .iter()
            .find(|line| line.kind == LineKind::Add)
            .unwrap();
        assert_eq!(added.file_path.as_deref(), Some("path with space.txt"));
    }
}
