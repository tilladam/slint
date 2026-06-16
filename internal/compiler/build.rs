// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0

use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

const FROZEN_ARTIFACT_MODULE_FILE: &str = "frozen_builtin_artifacts.rs";
const FROZEN_ARTIFACT_MANIFEST_FILE: &str = "frozen_builtin_artifacts.manifest";
const FROZEN_ARTIFACT_DIR: &str = "frozen-builtin-artifacts";
const FROZEN_ARTIFACT_MANIFEST_SCHEMA: &str = "frozen-builtin-artifacts-v0";
const FROZEN_ARTIFACT_SCHEMA_VERSION: &str = "3";

fn main() -> std::io::Result<()> {
    println!("cargo:rustc-check-cfg=cfg(slint_debug_property)");
    println!("cargo:rerun-if-env-changed=SLINT_FROZEN_BUILTIN_ARTIFACTS_DIR");
    println!("cargo:rerun-if-env-changed=SLINT_FROZEN_BUILTIN_ARTIFACTS_MODULE");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_FROZEN_BUILTIN_ARTIFACTS");

    let cargo_manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let library_dir = PathBuf::from("widgets");

    println!("cargo:rerun-if-changed={}", library_dir.display());

    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let output_file_path = out_dir.join(Path::new("included_library").with_extension("rs"));
    let frozen_artifact_output_file_path = out_dir.join(FROZEN_ARTIFACT_MODULE_FILE);

    let mut file = BufWriter::new(std::fs::File::create(&output_file_path)?);
    write!(
        file,
        r#"
fn widget_library() -> &'static [(&'static str, &'static BuiltinDirectory<'static>)] {{
    &[
"#
    )?;

    for style in cargo_manifest_dir.join(&library_dir).read_dir()?.filter_map(Result::ok) {
        if !style.file_type().is_ok_and(|f| f.is_dir()) {
            continue;
        }
        let path = style.path();
        writeln!(
            file,
            "(\"{}\", &[{}]),",
            path.file_name().unwrap().to_string_lossy(),
            process_style(&cargo_manifest_dir, &path)?
        )?;
    }

    writeln!(file, "]\n}}")?;
    file.flush()?;

    println!("cargo:rustc-env=SLINT_WIDGETS_LIBRARY={}", output_file_path.display());

    if let Some(generated_artifact_dir) = std::env::var_os("SLINT_FROZEN_BUILTIN_ARTIFACTS_DIR") {
        let generated_artifact_dir = PathBuf::from(generated_artifact_dir);
        copy_generated_frozen_builtin_artifacts(
            &generated_artifact_dir.join(FROZEN_ARTIFACT_MODULE_FILE),
            &out_dir,
            &frozen_artifact_output_file_path,
        )?;
    } else if let Some(generated_artifact_module) =
        std::env::var_os("SLINT_FROZEN_BUILTIN_ARTIFACTS_MODULE")
    {
        copy_generated_frozen_builtin_artifacts(
            &PathBuf::from(generated_artifact_module),
            &out_dir,
            &frozen_artifact_output_file_path,
        )?;
    } else if std::env::var_os("CARGO_FEATURE_FROZEN_BUILTIN_ARTIFACTS").is_some() {
        let generated_artifact_dir = cargo_manifest_dir.join(FROZEN_ARTIFACT_DIR);
        println!("cargo:rerun-if-changed={}", generated_artifact_dir.display());
        let generated_artifact_module = generated_artifact_dir.join(FROZEN_ARTIFACT_MODULE_FILE);
        if generated_artifact_module.exists() {
            copy_generated_frozen_builtin_artifacts(
                &generated_artifact_module,
                &out_dir,
                &frozen_artifact_output_file_path,
            )?;
        } else {
            write_empty_frozen_builtin_artifacts_module(&frozen_artifact_output_file_path)?;
        }
    } else {
        write_empty_frozen_builtin_artifacts_module(&frozen_artifact_output_file_path)?;
    }

    Ok(())
}

fn write_empty_frozen_builtin_artifacts_module(path: &Path) -> std::io::Result<()> {
    let mut frozen_artifact_file = BufWriter::new(std::fs::File::create(path)?);
    write!(
        frozen_artifact_file,
        r#"
pub(crate) fn generated_artifact(
    _key: &super::FrozenBuiltinCacheKey,
) -> Option<&'static [u8]> {{
    None
}}

pub(crate) fn artifact_count() -> usize {{
    0
}}
"#
    )?;
    frozen_artifact_file.flush()
}

fn copy_generated_frozen_builtin_artifacts(
    generated_artifact_module: &Path,
    out_dir: &Path,
    frozen_artifact_output_file_path: &Path,
) -> std::io::Result<()> {
    println!("cargo:rerun-if-changed={}", generated_artifact_module.display());
    let manifest_dir = generated_artifact_module.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} has no parent directory", generated_artifact_module.display()),
        )
    })?;
    let manifest_path = manifest_dir.join(FROZEN_ARTIFACT_MANIFEST_FILE);
    copy_generated_frozen_builtin_artifact_payloads(&manifest_path, out_dir)?;
    std::fs::copy(generated_artifact_module, frozen_artifact_output_file_path)?;
    Ok(())
}

fn copy_generated_frozen_builtin_artifact_payloads(
    manifest_path: &Path,
    out_dir: &Path,
) -> std::io::Result<()> {
    println!("cargo:rerun-if-changed={}", manifest_path.display());
    let manifest = std::fs::read_to_string(manifest_path)?;
    let manifest_dir = manifest_path.parent().unwrap_or_else(|| Path::new("."));
    let mut artifact_count = None;
    let mut artifact_paths = Vec::new();
    let mut has_manifest_schema = false;
    let mut has_artifact_schema_version = false;

    for line in manifest.lines() {
        if line == format!("schema={FROZEN_ARTIFACT_MANIFEST_SCHEMA}") {
            has_manifest_schema = true;
        } else if line == format!("artifact_schema_version={FROZEN_ARTIFACT_SCHEMA_VERSION}") {
            has_artifact_schema_version = true;
        } else if let Some(count) = line.strip_prefix("artifact_count=") {
            artifact_count = Some(count.parse::<usize>().map_err(|err| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("invalid artifact_count in {}: {err}", manifest_path.display()),
                )
            })?);
        } else if let Some(path) = line.strip_prefix("path=") {
            let path = PathBuf::from(path);
            let path = if path.is_absolute() { path } else { manifest_dir.join(path) };
            artifact_paths.push(path);
        }
    }

    if !has_manifest_schema {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{} has unsupported manifest schema", manifest_path.display()),
        ));
    }
    if !has_artifact_schema_version {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{} has unsupported artifact schema version", manifest_path.display()),
        ));
    }
    if artifact_count != Some(artifact_paths.len()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "{} artifact_count does not match listed artifact paths",
                manifest_path.display()
            ),
        ));
    }

    for path in artifact_paths {
        println!("cargo:rerun-if-changed={}", path.display());
        let file_name = path.file_name().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("artifact path {} has no file name", path.display()),
            )
        })?;
        std::fs::copy(&path, out_dir.join(file_name))?;
    }
    Ok(())
}

fn process_style(cargo_manifest_dir: &Path, path: &Path) -> std::io::Result<String> {
    let library_files: Vec<PathBuf> = cargo_manifest_dir
        .join(path)
        .read_dir()?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.file_type().is_ok_and(|f| !f.is_dir())
                && entry
                    .path()
                    .extension()
                    .map(|ext| {
                        ext == std::ffi::OsStr::new("slint")
                            || ext == std::ffi::OsStr::new("60")
                            || ext == std::ffi::OsStr::new("svg")
                            || ext == std::ffi::OsStr::new("svgz")
                    })
                    .unwrap_or_default()
        })
        .map(|entry| entry.path())
        .collect();

    Ok(library_files
        .iter()
        .map(|file| {
            format!(
                "&BuiltinFile {{path: r#\"{}\"# , contents: include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), r#\"/{}\"#))}}",
                file.file_name().unwrap().to_string_lossy(),
                file.strip_prefix(cargo_manifest_dir).unwrap().display()
            )
        })
        .collect::<Vec<_>>()
        .join(","))
}
