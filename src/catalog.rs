//! Resolve a source and load every script in it: file, content, metadata. Blocking (git,
//! filesystem); the TUI runs it on a blocking task.

use std::path::Path;

use anyhow::{Context, Result};

use crate::discovery::metadata::{self, Metadata};
use crate::discovery::{self, ScriptFile};
use crate::source::{self, ResolvedSource};

#[derive(Debug, Clone)]
pub struct Catalog {
    pub source: ResolvedSource,
    /// Sorted by ID.
    pub scripts: Vec<Script>,
    /// Non-fatal problems from resolving and scanning.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script {
    pub file: ScriptFile,
    pub content: String,
    pub meta: Metadata,
}

impl Script {
    pub fn new(file: ScriptFile, content: String) -> Self {
        let meta = metadata::extract(&content);
        Self { file, content, meta }
    }
}

/// Resolve `input` (cloning/updating git sources) and scan it.
pub fn load(input: &str, cache_root: &Path) -> Result<Catalog> {
    let source = source::resolve(input, cache_root).with_context(|| format!("resolving {input}"))?;
    scan(source)
}

/// Scan an already resolved source again (after edits); never touches git.
pub fn scan(source: ResolvedSource) -> Result<Catalog> {
    let found = match &source.file {
        Some(file) => discovery::single_file(file)?,
        None => discovery::walk(&source.root)?,
    };
    let mut warnings: Vec<String> = source.warnings.iter().chain(&found.warnings).cloned().collect();
    let mut scripts = Vec::with_capacity(found.scripts.len());
    for file in found.scripts {
        match std::fs::read(&file.path) {
            Ok(bytes) => scripts.push(Script::new(file, String::from_utf8_lossy(&bytes).into_owned())),
            Err(e) => warnings.push(format!("cannot read {}: {e}", file.id)),
        }
    }
    Ok(Catalog {
        source,
        scripts,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/scripts");

    #[test]
    fn loads_fixtures_with_metadata() {
        let catalog = load(FIXTURES, Path::new("/unused")).expect("load");
        assert_eq!(catalog.scripts.len(), 8);
        assert!(catalog.warnings.is_empty(), "{:?}", catalog.warnings);
        let params = catalog
            .scripts
            .iter()
            .find(|s| s.file.id == "params_demo.bt")
            .expect("params_demo");
        assert_eq!(params.meta.params.positional, vec![1]);
        assert!(params.content.contains("getopt(\"verbose\")"));

        let again = scan(catalog.source.clone()).expect("rescan");
        assert_eq!(again.scripts, catalog.scripts);
    }
}
