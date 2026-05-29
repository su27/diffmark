use crate::diff_parser::{DiffLine, ParsedDiff};

pub fn describe_target(line: &DiffLine) -> String {
    match (line.old_lineno, line.new_lineno) {
        (Some(old), Some(new)) => format!("context line old:{old} new:{new}"),
        (None, Some(new)) => format!("added line new:{new}"),
        (Some(old), None) => format!("deleted line old:{old}"),
        (None, None) => "diff line".to_string(),
    }
}

pub fn build_review_payload(
    parsed: &ParsedDiff,
    target_lines: &[DiffLine],
    comment: &str,
    max_hunk_lines: usize,
) -> String {
    let Some(first_line) = target_lines.first() else {
        return String::new();
    };
    let file_path = first_line.file_path.as_deref().unwrap_or("(unknown file)");
    let mut output = vec![
        format!("File: {file_path}"),
        format!("My comment at {}", comment_line_label(target_lines)),
        String::new(),
    ];

    if let Some(mut hunk_lines) = format_hunk_window(parsed, target_lines, comment, max_hunk_lines)
    {
        output.append(&mut hunk_lines);
    } else {
        output.push(">> target:".to_string());
        output.extend(target_lines.iter().map(|line| line.raw.clone()));
        output.extend(format_comment(comment));
    }

    output.push(String::new());
    output.join("\n")
}

fn format_hunk_window(
    parsed: &ParsedDiff,
    target_lines: &[DiffLine],
    comment: &str,
    max_hunk_lines: usize,
) -> Option<Vec<String>> {
    let first_line = target_lines.first()?;
    let last_line = target_lines.last()?;
    let hunk_index = first_line.hunk_index?;
    if !target_lines
        .iter()
        .all(|line| line.hunk_index == Some(hunk_index))
    {
        return None;
    }
    let target_start = first_line.hunk_line_index?;
    let target_end = last_line.hunk_line_index?;
    let (target_start, target_end) = normalized_range(target_start, target_end);
    let hunk = parsed.hunks.get(hunk_index)?;
    let hunk_lines = &hunk.line_indices;
    let max_hunk_lines = max_hunk_lines.max(1);
    let target_line_count = target_end - target_start + 1;
    let window_lines = max_hunk_lines.max(target_line_count);

    let (start, end) = if hunk_lines.len() <= window_lines {
        (0, hunk_lines.len())
    } else {
        let spare = window_lines.saturating_sub(target_line_count);
        let before = spare / 2;
        let mut start = target_start.saturating_sub(before);
        let mut end = (start + window_lines).min(hunk_lines.len());
        if end <= target_end {
            end = (target_end + 1).min(hunk_lines.len());
            start = end.saturating_sub(window_lines);
        }
        end = (start + window_lines).min(hunk_lines.len());
        (start, end)
    };

    let mut output = vec![hunk.header.clone()];
    if start > 0 {
        output.push("...".to_string());
    }

    for local_index in start..end {
        let line_index = *hunk_lines.get(local_index)?;
        let current = parsed.lines.get(line_index)?;
        if local_index == target_start {
            output.push(">> target:".to_string());
        }
        if local_index >= target_start && local_index <= target_end {
            output.push(current.raw.clone());
            if local_index == target_end {
                output.extend(format_comment(comment));
            }
        } else {
            output.push(current.raw.clone());
        }
    }

    Some(output)
}

fn comment_line_label(lines: &[DiffLine]) -> String {
    match lines {
        [] => "line unknown".to_string(),
        [line] => format!("line {}", line_number(line)),
        lines => format!(
            "lines {}-{}",
            line_number(lines.first().unwrap()),
            line_number(lines.last().unwrap())
        ),
    }
}

fn line_number(line: &DiffLine) -> String {
    line.new_lineno
        .or(line.old_lineno)
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn normalized_range(a: usize, b: usize) -> (usize, usize) {
    if a <= b { (a, b) } else { (b, a) }
}

fn format_comment(comment: &str) -> Vec<String> {
    let lines: Vec<&str> = comment.trim_end().lines().collect();
    if lines.len() == 1 {
        return vec![format!(">> comment: {}", lines[0])];
    }

    let mut output = vec![">> comment:".to_string()];
    output.extend(lines.into_iter().map(|line| format!(">> {line}")));
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff_parser::{LineKind, parse_unified_diff};

    fn make_diff(line_count: usize) -> String {
        let mut lines = vec![
            "diff --git a/big.txt b/big.txt".to_string(),
            "--- a/big.txt".to_string(),
            "+++ b/big.txt".to_string(),
            format!("@@ -1,{line_count} +1,{line_count} @@"),
        ];
        for index in 1..=line_count {
            let prefix = if index == 10 { '+' } else { ' ' };
            lines.push(format!("{prefix}line {index}"));
        }
        lines.join("\n") + "\n"
    }

    #[test]
    fn marks_selected_line_and_comment_insertion() {
        let parsed = parse_unified_diff(&make_diff(20));
        let target = parsed
            .lines
            .iter()
            .find(|line| line.kind == LineKind::Add && line.raw == "+line 10")
            .unwrap();

        let payload = build_review_payload(
            &parsed,
            &[target.clone()],
            "Please revisit this.\nSecond line.",
            11,
        );

        assert!(payload.contains("File: big.txt"));
        assert!(payload.contains("My comment at line 10"));
        assert!(payload.contains(">> target:\n+line 10"));
        assert!(payload.contains("+line 10"));
        assert!(payload.contains(">> comment:\n>> Please revisit this.\n>> Second line."));
        assert!(payload.contains("Second line."));
        assert!(!payload.contains("VDIFF_SELECTED_LINE"));
        assert!(!payload.contains("```"));
    }

    #[test]
    fn trims_long_hunks_around_target() {
        let parsed = parse_unified_diff(&make_diff(200));
        let target = parsed
            .lines
            .iter()
            .find(|line| line.kind == LineKind::Add && line.raw == "+line 10")
            .unwrap();

        let payload = build_review_payload(&parsed, &[target.clone()], "Trim this.", 11);

        assert!(payload.contains("+line 10"));
        assert!(payload.contains(">> target:\n+line 10"));
        assert!(payload.contains(">> comment: Trim this."));
        assert!(!payload.contains(" line 200"));
    }

    #[test]
    fn keeps_all_selected_lines_in_multi_line_payload() {
        let parsed = parse_unified_diff(&make_diff(200));
        let targets: Vec<_> = parsed
            .lines
            .iter()
            .filter(|line| {
                line.hunk_line_index
                    .is_some_and(|index| (8..=12).contains(&index))
            })
            .cloned()
            .collect();

        let payload = build_review_payload(&parsed, &targets, "Block comment.", 3);

        assert!(payload.contains("My comment at lines 9-13"));
        assert!(payload.contains(">> target:\n line 9\n+line 10\n line 11\n line 12\n line 13"));
        assert!(payload.contains(">> comment: Block comment."));
        assert!(!payload.contains(" line 200"));
    }
}
