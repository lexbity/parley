//! Canonical Markdown task indexing and byte-preserving checkbox updates.
//!
//! Offsets use canonical UTF-8 bytes. Only CommonMark list items outside code
//! and block quotes are tasks; task updates preserve every byte outside the
//! addressed marker or inserted checkbox.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    /// Start of the addressed LF line in canonical UTF-8 bytes.
    pub byte_start: usize,
    /// End of the addressed LF line, excluding its LF byte.
    pub byte_end: usize,
    /// SHA-256 of the complete line, excluding LF.
    pub line_sha256: String,
    pub text: String,
    pub checked: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToggleResult {
    pub markdown: String,
    pub tasks: Vec<Task>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Fence {
    marker: u8,
    length: usize,
}

fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn fence_line(line: &[u8]) -> Option<Fence> {
    let indent = line.iter().take_while(|byte| **byte == b' ').count();
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    let marker = *rest.first()?;
    if marker != b'`' && marker != b'~' {
        return None;
    }
    let count = rest.iter().take_while(|byte| **byte == marker).count();
    (count >= 3).then_some(Fence {
        marker,
        length: count,
    })
}

fn closes_fence(line: &[u8], fence: Fence) -> bool {
    let indent = line.iter().take_while(|byte| **byte == b' ').count();
    if indent > 3 {
        return false;
    }
    let rest = &line[indent..];
    let count = rest
        .iter()
        .take_while(|byte| **byte == fence.marker)
        .count();
    count >= fence.length && rest[count..].iter().all(u8::is_ascii_whitespace)
}

fn list_content_start(line: &[u8]) -> Option<usize> {
    let indent = line.iter().take_while(|byte| **byte == b' ').count();
    if indent > 3 || line.get(indent) == Some(&b'\t') {
        return None;
    }
    let mut marker_end = indent;
    match *line.get(indent)? {
        b'-' | b'+' | b'*' => marker_end += 1,
        b'0'..=b'9' => {
            while line.get(marker_end).is_some_and(u8::is_ascii_digit) && marker_end - indent < 9 {
                marker_end += 1;
            }
            if !matches!(line.get(marker_end), Some(b'.' | b')')) {
                return None;
            }
            marker_end += 1;
        }
        _ => return None,
    }
    if !line.get(marker_end).is_some_and(u8::is_ascii_whitespace) {
        return None;
    }
    Some(marker_end + 1)
}

fn task_on_line(line: &[u8], byte_start: usize) -> Option<Task> {
    if line.iter().take_while(|byte| **byte == b' ').count() >= 4
        || line.iter().take_while(|byte| **byte == b' ').count() < line.len()
            && line[line.iter().take_while(|byte| **byte == b' ').count()] == b'>'
    {
        return None;
    }
    let content_start = list_content_start(line)?;
    if line.get(content_start..content_start + 2) == Some(b"\\[") {
        return None;
    }
    let (checked, text_start) = if line.get(content_start..content_start + 3) == Some(b"[ ]")
        && line
            .get(content_start + 3)
            .is_none_or(u8::is_ascii_whitespace)
    {
        (
            false,
            content_start + 3 + usize::from(line.get(content_start + 3).is_some()),
        )
    } else if line.get(content_start..content_start + 3) == Some(b"[x]")
        || line.get(content_start..content_start + 3) == Some(b"[X]")
    {
        if line
            .get(content_start + 3)
            .is_some_and(|byte| !byte.is_ascii_whitespace())
        {
            return None;
        }
        (
            true,
            content_start + 3 + usize::from(line.get(content_start + 3).is_some()),
        )
    } else {
        (false, content_start)
    };
    let text = String::from_utf8_lossy(&line[text_start..])
        .trim()
        .to_owned();
    let offset = if text.is_empty() {
        text_start
    } else {
        let raw = &line[text_start..];
        text_start
            + raw
                .iter()
                .take_while(|byte| byte.is_ascii_whitespace())
                .count()
    };
    Some(Task {
        byte_start,
        byte_end: byte_start + line.len(),
        line_sha256: hash(line),
        text: String::from_utf8_lossy(&line[offset..]).trim().to_owned(),
        checked,
    })
}

/// Index tasks while excluding fenced/indented code and block quotes.
pub fn index(markdown: &str) -> Vec<Task> {
    let bytes = markdown.as_bytes();
    let mut tasks = Vec::new();
    let mut cursor = 0usize;
    let mut fence = None;
    for part in bytes.split_inclusive(|byte| *byte == b'\n') {
        let line = part.strip_suffix(b"\n").unwrap_or(part);
        if let Some(active) = fence {
            if closes_fence(line, active) {
                fence = None;
            }
        } else if let Some(found) = fence_line(line) {
            fence = Some(found);
        } else if let Some(task) = task_on_line(line, cursor) {
            tasks.push(task);
        }
        cursor += part.len();
    }
    tasks
}

/// Canonicalize CRLF/CR input to LF without changing any other UTF-8 bytes.
pub fn canonicalize(markdown: &str) -> String {
    markdown.replace("\r\n", "\n").replace('\r', "\n")
}

/// Change exactly one parsed task using a current byte span and line digest.
pub fn toggle(
    markdown: &str,
    byte_start: usize,
    byte_end: usize,
    line_sha256: &str,
    checked: bool,
) -> Result<ToggleResult, &'static str> {
    let bytes = markdown.as_bytes();
    if byte_start > byte_end
        || byte_end > bytes.len()
        || !markdown.is_char_boundary(byte_start)
        || !markdown.is_char_boundary(byte_end)
    {
        return Err("task span is out of bounds or not on UTF-8 boundaries");
    }
    if bytes[byte_end..].first().is_some_and(|byte| *byte != b'\n')
        || bytes[..byte_start]
            .last()
            .is_some_and(|byte| *byte != b'\n')
    {
        return Err("task span must address one complete LF line");
    }
    let line = &bytes[byte_start..byte_end];
    if hash(line) != line_sha256 {
        return Err("task line changed; reload notes before toggling");
    }
    let task = task_on_line(line, byte_start).ok_or("addressed Markdown line is not a task")?;
    // The task index excludes fences, block quotes, indented code and escaped
    // markers (QV/notes scope). A toggle must address a task that the index
    // actually recognizes; otherwise a fenced checkbox would be mutable.
    if !index(markdown)
        .iter()
        .any(|indexed| indexed.byte_start == byte_start && indexed.byte_end == byte_end)
    {
        return Err("addressed line is inside a fence, quote or code block and cannot be toggled");
    }
    let content_start =
        list_content_start(line).ok_or("addressed Markdown line is not a list item")?;
    let current_marker_end = if line.get(content_start..content_start + 3) == Some(b"[ ]")
        || line.get(content_start..content_start + 3) == Some(b"[x]")
        || line.get(content_start..content_start + 3) == Some(b"[X]")
    {
        content_start + 3
    } else {
        content_start
    };
    let mut updated = Vec::with_capacity(bytes.len() + 4);
    updated.extend_from_slice(&bytes[..byte_start]);
    if current_marker_end == content_start {
        updated.extend_from_slice(&line[..content_start]);
        updated.extend_from_slice(if checked { b"[x] " } else { b"[ ] " });
        updated.extend_from_slice(&line[content_start..]);
    } else {
        updated.extend_from_slice(&line[..content_start + 1]);
        updated.push(if checked { b'x' } else { b' ' });
        updated.extend_from_slice(&line[content_start + 2..]);
    }
    updated.extend_from_slice(&bytes[byte_end..]);
    let markdown = String::from_utf8(updated).map_err(|_| "task update produced invalid UTF-8")?;
    let tasks = index(&markdown);
    let _ = task;
    Ok(ToggleResult { markdown, tasks })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_index_excludes_code_quotes_and_escaped_marker_text() {
        let markdown = "- open\n- [x] done\n> - quoted\n\n    - indented code\n```md\n- fenced\n```\n- \\[ \\] literal\n";
        let tasks = index(markdown);
        assert_eq!(
            tasks
                .iter()
                .map(|task| task.text.as_str())
                .collect::<Vec<_>>(),
            ["open", "done"]
        );
        assert!(!tasks[0].checked);
        assert!(tasks[1].checked);
    }

    #[test]
    fn toggle_changes_only_one_marker_and_preserves_nested_bytes() {
        let markdown = "1.  Parent\n  - [ ]  Nested 😀\n```text\n- [ ] example\n```\n";
        let tasks = index(markdown);
        assert_eq!(tasks.len(), 2);
        let task = &tasks[1];
        let result = toggle(
            markdown,
            task.byte_start,
            task.byte_end,
            &task.line_sha256,
            true,
        )
        .unwrap();
        assert_eq!(
            result.markdown,
            "1.  Parent\n  - [x]  Nested 😀\n```text\n- [ ] example\n```\n"
        );
        assert_eq!(result.markdown.len(), markdown.len());
    }

    #[test]
    fn toggle_inserts_checkbox_after_list_prefix_and_rejects_stale_input() {
        let markdown = "-  Draft task\n";
        let task = &index(markdown)[0];
        let result = toggle(
            markdown,
            task.byte_start,
            task.byte_end,
            &task.line_sha256,
            false,
        )
        .unwrap();
        assert_eq!(result.markdown, "- [ ]  Draft task\n");
        assert!(toggle(markdown, task.byte_start, task.byte_end, "stale", true).is_err());
    }

    #[test]
    fn toggle_rejects_a_checkbox_inside_a_fenced_code_block() {
        // The fence exclusion lives in index(); a toggle must not bypass it by
        // addressing bytes directly (RFC-0004 notes fixture discipline: "a
        // literal Markdown checkbox inside a fenced code block").
        let markdown = "```md\n- [ ] seeded\n```\n";
        // Locate the fenced line's bytes directly, as a malicious client would.
        let fence_start = markdown.find("- [ ] seeded").unwrap();
        let fence_end = fence_start + "- [ ] seeded".len();
        let line_hash = hash(&markdown.as_bytes()[fence_start..fence_end]);
        let err = toggle(markdown, fence_start, fence_end, &line_hash, true).unwrap_err();
        assert!(err.contains("fence"), "{err}");
        assert_eq!(markdown, "```md\n- [ ] seeded\n```\n");
    }
}
