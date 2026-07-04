use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug)]
struct SourceHit {
    path: String,
    line: usize,
    text: String,
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("proj-core should live inside the workspace")
        .to_path_buf()
}

fn collect_rust_sources(dir: &Path, sources: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rust_sources(&path, sources);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            sources.push(path);
        }
    }
}

fn audited_sources() -> Vec<PathBuf> {
    let root = workspace_root();
    let mut sources = Vec::new();
    for crate_dir in ["proj-core/src", "proj-wkt/src", "gen-reference/src"] {
        collect_rust_sources(&root.join(crate_dir), &mut sources);
    }
    sources
}

fn relative_path(path: &Path) -> String {
    path.strip_prefix(workspace_root())
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn source_hits(needle: &str) -> Vec<SourceHit> {
    audited_sources()
        .into_iter()
        .flat_map(|path| {
            let relative = relative_path(&path);
            fs::read_to_string(&path)
                .expect("source file should be readable")
                .lines()
                .enumerate()
                .filter(|(_, line)| line.contains(needle))
                .map(move |(index, line)| SourceHit {
                    path: relative.clone(),
                    line: index + 1,
                    text: line.trim().to_string(),
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

fn format_hits(hits: &[SourceHit]) -> String {
    hits.iter()
        .map(|hit| format!("{}:{}: {}", hit.path, hit.line, hit.text))
        .collect::<Vec<_>>()
        .join("\n")
}

fn assert_no_hits(needle: &str) {
    let hits = source_hits(needle);
    assert!(
        hits.is_empty(),
        "unexpected source hits for `{needle}`:\n{}",
        format_hits(&hits)
    );
}

fn assert_hits_allowed(needle: &str, is_allowed: impl Fn(&SourceHit) -> bool) {
    let unexpected = source_hits(needle)
        .into_iter()
        .filter(|hit| !is_allowed(hit))
        .collect::<Vec<_>>();
    assert!(
        unexpected.is_empty(),
        "unexpected source hits for `{needle}`:\n{}",
        format_hits(&unexpected)
    );
}

#[test]
fn selector_operation_kind_variants_are_registry_custom_or_identity() {
    let selector = fs::read_to_string(workspace_root().join("proj-core/src/selector.rs"))
        .expect("selector source should be readable");
    let enum_start = selector
        .find("enum SelectedOperationKind")
        .expect("SelectedOperationKind enum should exist");
    let block_start = selector[enum_start..]
        .find('{')
        .map(|index| enum_start + index + 1)
        .expect("SelectedOperationKind enum should have a body");
    let block_end = selector[block_start..]
        .find("\n}")
        .map(|index| block_start + index)
        .expect("SelectedOperationKind enum body should close");

    let variants = selector[block_start..block_end]
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            line.split(['(', ','])
                .next()
                .expect("variant line should not be empty")
        })
        .collect::<Vec<_>>();

    assert_eq!(variants, ["Identity", "Registry", "Custom"]);
}

#[test]
fn transform_sources_do_not_use_synthetic_operation_helpers() {
    assert_no_hits("synthetic_");
}

#[test]
fn coordinate_operation_literals_stay_in_registry_parser_or_custom_paths() {
    assert_hits_allowed("CoordinateOperation {", |hit| {
        matches!(
            hit.path.as_str(),
            "proj-core/src/epsg_db.rs"
                | "proj-core/src/operation.rs"
                | "proj-core/src/registry.rs"
                | "proj-core/src/transform/tests.rs"
                | "proj-wkt/src/lib.rs"
        )
    });
}

#[test]
fn helmert_and_datum_shift_methods_are_not_candidate_synthesis_paths() {
    assert_hits_allowed("OperationMethod::Helmert", |hit| {
        matches!(
            hit.path.as_str(),
            "proj-core/src/epsg_db.rs"
                | "proj-core/src/transform/pipeline.rs"
                | "proj-core/src/transform/tests.rs"
        )
    });

    assert_hits_allowed("OperationMethod::DatumShift", |hit| {
        matches!(
            hit.path.as_str(),
            "proj-core/src/operation.rs"
                | "proj-core/src/transform/pipeline.rs"
                | "proj-wkt/src/lib.rs"
        )
    });
}
