//! Scenario loading. scenarios/*.yaml keep the fixed flat `key: value`
//! structure (kind/metric/unit/command/description/notes + the
//! resource-footprint key pairs), read by a small line parser.
//!
//! The scenario files are compiled into the binary, so an installed
//! `tbench` works from any directory. A `scenarios/` directory next to the
//! binary or in the working directory takes precedence, which is what a
//! checkout uses.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Scenario {
    pub name: String, // file stem, e.g. "shell-startup"
    pub path: PathBuf,
    fields: HashMap<String, String>,
}

macro_rules! embedded {
    ($($name:literal),* $(,)?) => {
        &[$(($name, include_str!(concat!("../scenarios/", $name, ".yaml")))),*]
    };
}

/// The bundled scenarios, in run order (file name order).
const EMBEDDED: &[(&str, &str)] = embedded![
    "cmd-overhead",
    "fire",
    "render-throughput",
    "resource-footprint",
    "scroll-proxy",
    "shell-startup",
];

impl Scenario {
    pub fn load(path: &Path) -> std::io::Result<Scenario> {
        let text = std::fs::read_to_string(path)?;
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        Ok(Scenario::parse(name, &text, path.to_path_buf()))
    }

    fn parse(name: String, text: &str, path: PathBuf) -> Scenario {
        let mut fields = HashMap::new();
        for line in text.lines() {
            // First `key: value` wins, matching `head -n1` in the zsh parser.
            if let Some((key, value)) = line.split_once(':') {
                let key = key.trim();
                if key.is_empty() || key.contains(char::is_whitespace) {
                    continue;
                }
                let mut value = value.trim().to_string();
                if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
                    value = value[1..value.len() - 1].to_string();
                }
                fields.entry(key.to_string()).or_insert(value);
            }
        }
        Scenario { name, path, fields }
    }

    pub fn field(&self, key: &str) -> Option<&str> {
        self.fields
            .get(key)
            .map(|s| s.as_str())
            .filter(|s| !s.is_empty())
    }

    pub fn kind(&self) -> &str {
        self.field("kind").unwrap_or("shell")
    }
}

pub fn embedded() -> Vec<Scenario> {
    EMBEDDED
        .iter()
        .map(|(name, text)| {
            Scenario::parse(
                name.to_string(),
                text,
                PathBuf::from(format!("<built-in>/{name}.yaml")),
            )
        })
        .collect()
}

/// All scenarios, sorted by file name, which fixes the run order: those in
/// `root/scenarios/` when that directory exists, the bundled ones otherwise.
pub fn load_all(root: &Path) -> std::io::Result<Vec<Scenario>> {
    let dir = root.join("scenarios");
    if !dir.is_dir() {
        return Ok(embedded());
    }
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "yaml"))
        .collect();
    paths.sort();
    paths.iter().map(|p| Scenario::load(p)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_scenarios_match_the_files_and_are_in_run_order() {
        let bundled = embedded();
        let names: Vec<&str> = bundled.iter().map(|s| s.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);

        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let on_disk = load_all(root).unwrap();
        let disk_names: Vec<&str> = on_disk.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names, disk_names,
            "a scenario file is missing from EMBEDDED"
        );
        for (a, b) in bundled.iter().zip(&on_disk) {
            assert_eq!(a.kind(), b.kind());
            assert_eq!(a.field("metric"), b.field("metric"));
        }
    }

    #[test]
    fn falls_back_to_bundled_scenarios_without_a_directory() {
        let empty = std::env::temp_dir().join(format!("tbench-noscn-{}", std::process::id()));
        std::fs::create_dir_all(&empty).unwrap();
        let loaded = load_all(&empty).unwrap();
        assert_eq!(loaded.len(), EMBEDDED.len());
        assert!(loaded[0].path.starts_with("<built-in>"));
        let _ = std::fs::remove_dir_all(empty);
    }

    #[test]
    fn parser_takes_the_first_key_strips_quotes_and_skips_noise() {
        let s = Scenario::parse(
            "x".into(),
            "metric: a.b\nmetric: ignored\nunit: \"ms\"\ncommand: \"{shell} -c true\"\nnot a key: v\n# comment\nempty:\n",
            PathBuf::from("x.yaml"),
        );
        assert_eq!(s.field("metric"), Some("a.b"));
        assert_eq!(s.field("unit"), Some("ms"));
        assert_eq!(s.field("command"), Some("{shell} -c true"));
        assert_eq!(s.field("not a key"), None);
        assert_eq!(s.field("empty"), None);
        assert_eq!(s.kind(), "shell");
    }
}
