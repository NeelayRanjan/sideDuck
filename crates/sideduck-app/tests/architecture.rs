//! Source-level architecture constraints from the product specification.
use std::path::{Path, PathBuf};
fn rust_sources(directory: &Path, output: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_sources(&path, output);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            output.push(path);
        }
    }
}
#[test]
fn only_platform_crate_contains_target_os_configuration() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    for entry in std::fs::read_dir(crates).unwrap() {
        let path = entry.unwrap().path();
        if path
            .file_name()
            .is_some_and(|name| name == "sideduck-platform")
        {
            continue;
        }
        let source = path.join("src");
        if !source.exists() {
            continue;
        }
        let mut files = Vec::new();
        rust_sources(&source, &mut files);
        for file in files {
            let text = std::fs::read_to_string(&file).unwrap();
            for attribute in text
                .split("#[")
                .skip(1)
                .filter_map(|part| part.split_once(']').map(|(a, _)| a))
            {
                assert!(
                    !attribute.contains("target_os"),
                    "native operating-system configuration escaped platform boundary: {}",
                    file.display()
                );
            }
        }
    }
}
#[test]
fn motifs_are_not_used_for_technical_identifiers() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    for entry in std::fs::read_dir(crates).unwrap() {
        let source = entry.unwrap().path().join("src");
        if !source.exists() {
            continue;
        }
        let mut files = Vec::new();
        rust_sources(&source, &mut files);
        for file in files {
            let text = std::fs::read_to_string(&file).unwrap();
            for line in text
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
            {
                let tokens: Vec<_> = line
                    .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
                    .filter(|token| !token.is_empty())
                    .collect();
                for pair in tokens.windows(2) {
                    if ["fn", "struct", "enum", "type", "mod", "trait"].contains(&pair[0]) {
                        let name = pair[1].to_ascii_lowercase();
                        // The crate prefix is mandated explicitly by the spec.
                        if name.starts_with("sideduck_") {
                            continue;
                        }
                        for motif in ["duck", "quack", "paddle", "flock", "pond", "quill"] {
                            assert!(
                                !name.contains(motif),
                                "motif '{motif}' in technical identifier {} at {}",
                                pair[1],
                                file.display()
                            );
                        }
                    }
                }
            }
        }
    }
}
