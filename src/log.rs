use crate::paths::Paths;
use std::collections::VecDeque;
use std::io::{self, Write};

pub fn append_log(p: &Paths, id: &str, chunk: &[u8]) -> io::Result<()> {
    let path = p.log(id);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    f.write_all(chunk)
}

pub fn rotate_if_needed(p: &Paths, id: &str, max_bytes: u64) {
    let path = p.log(id);
    if matches!(std::fs::metadata(&path), Ok(m) if m.len() > max_bytes) {
        let mut rotated = path.clone().into_os_string();
        rotated.push(".1");
        let _ = std::fs::rename(&path, rotated);
    }
}

/// Keeps the last N lines of a stream, including a trailing partial line.
/// Lines are kept as raw bytes and decoded lossily in `text()`, so a multibyte
/// character split across pushes decodes correctly.
pub struct TailBuffer {
    max: usize,
    lines: VecDeque<Vec<u8>>,
    partial: Vec<u8>,
}

impl TailBuffer {
    pub fn new(max: usize) -> TailBuffer {
        TailBuffer {
            max,
            lines: VecDeque::new(),
            partial: Vec::new(),
        }
    }

    pub fn push(&mut self, chunk: &[u8]) {
        for part in chunk.split_inclusive(|&b| b == b'\n') {
            if let Some(body) = part.strip_suffix(b"\n") {
                self.partial.extend_from_slice(body);
                self.lines.push_back(std::mem::take(&mut self.partial));
                if self.lines.len() > self.max {
                    self.lines.pop_front();
                }
            } else {
                self.partial.extend_from_slice(part);
            }
        }
    }

    pub fn text(&self) -> String {
        let mut all: Vec<&[u8]> = self.lines.iter().map(Vec::as_slice).collect();
        if !self.partial.is_empty() {
            all.push(&self.partial);
        }
        let skip = all.len().saturating_sub(self.max);
        String::from_utf8_lossy(&all[skip..].join(&b'\n')).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(max: usize, chunks: &[&[u8]]) -> String {
        let mut t = TailBuffer::new(max);
        for c in chunks {
            t.push(c);
        }
        t.text()
    }

    #[test]
    fn tail_keeps_the_last_20_lines_including_a_partial_last_line() {
        let mut input = String::new();
        for i in 1..=50 {
            input.push_str(&format!("{i}\n"));
        }
        input.push_str("tail-no-newline");
        let text = feed(20, &[input.as_bytes()]);
        let lines: Vec<&str> = text.split('\n').collect();
        assert_eq!(lines.len(), 20);
        assert_eq!(lines[19], "tail-no-newline");
        assert_eq!(lines[0], "32");
    }

    #[test]
    fn tail_joins_lines_across_pushes() {
        assert_eq!(feed(20, &[b"ab", b"c\nd", b"e\n"]), "abc\nde");
        assert_eq!(feed(20, &[]), "");
    }

    #[test]
    fn tail_multibyte_split_across_pushes() {
        let bytes = "é─\n".as_bytes();
        assert_eq!(feed(20, &[&bytes[..1], &bytes[1..2], &bytes[2..]]), "é─");
    }

    #[test]
    fn tail_invalid_utf8_is_lossy() {
        assert_eq!(feed(20, &[b"a\xffb\n"]), "a\u{fffd}b");
    }

    #[test]
    fn log_rotates_past_1_mb() {
        let d = tempfile::tempdir().unwrap();
        let p = Paths::temp(d.path());
        append_log(&p, "t", &vec![b'x'; 1_150_000]).unwrap();
        rotate_if_needed(&p, "t", 1_048_576);
        append_log(&p, "t", b"hi\n").unwrap();
        assert!(p.log("t").with_extension("log.1").exists());
        assert!(std::fs::metadata(p.log("t")).unwrap().len() < 1_048_576);
    }

    #[test]
    fn no_rotation_at_or_below_limit_or_when_missing() {
        let d = tempfile::tempdir().unwrap();
        let p = Paths::temp(d.path());
        rotate_if_needed(&p, "t", 10);
        append_log(&p, "t", b"0123456789").unwrap();
        rotate_if_needed(&p, "t", 10);
        assert!(p.log("t").exists());
        assert!(!p.log("t").with_extension("log.1").exists());
    }
}
