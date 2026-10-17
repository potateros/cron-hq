use crate::paths::Paths;
use std::collections::BTreeSet;
use std::io;

pub fn read_paused(p: &Paths) -> BTreeSet<String> {
    let Ok(text) = std::fs::read_to_string(p.state()) else {
        return BTreeSet::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return BTreeSet::new();
    };
    match value.get("paused").and_then(|v| v.as_array()) {
        Some(items) => items
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        None => BTreeSet::new(),
    }
}

pub fn set_paused(p: &Paths, id: &str, paused: bool) -> io::Result<()> {
    let mut set = read_paused(p);
    if paused {
        set.insert(id.to_string());
    } else {
        set.remove(id);
    }
    let path = p.state();
    std::fs::create_dir_all(&p.home)?;
    let mut tmp = path.clone().into_os_string();
    tmp.push(".tmp");
    let body = serde_json::to_string_pretty(&serde_json::json!({ "paused": set }))
        .map_err(io::Error::other)?;
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, &path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> (tempfile::TempDir, Paths) {
        let d = tempfile::tempdir().unwrap();
        let p = Paths::temp(d.path());
        (d, p)
    }

    #[test]
    fn set_paused_persists_across_reads() {
        let (_d, p) = paths();
        assert!(read_paused(&p).is_empty());
        set_paused(&p, "a", true).unwrap();
        set_paused(&p, "b", true).unwrap();
        set_paused(&p, "a", false).unwrap();
        assert_eq!(read_paused(&p).into_iter().collect::<Vec<_>>(), ["b"]);
    }

    #[test]
    fn corrupt_state_json_is_empty_set() {
        let (_d, p) = paths();
        std::fs::create_dir_all(&p.home).unwrap();
        std::fs::write(p.state(), "{not json").unwrap();
        assert!(read_paused(&p).is_empty());
    }

    #[test]
    fn state_json_format() {
        let (_d, p) = paths();
        set_paused(&p, "a", true).unwrap();
        set_paused(&p, "b", true).unwrap();
        assert_eq!(
            std::fs::read_to_string(p.state()).unwrap(),
            "{\n  \"paused\": [\n    \"a\",\n    \"b\"\n  ]\n}"
        );
        assert!(!p.state().with_extension("json.tmp").exists());
    }

    #[test]
    fn non_string_entries_are_ignored() {
        let (_d, p) = paths();
        std::fs::create_dir_all(&p.home).unwrap();
        std::fs::write(p.state(), r#"{"paused":["a",1,null,"b"]}"#).unwrap();
        assert_eq!(read_paused(&p).len(), 2);
    }
}
