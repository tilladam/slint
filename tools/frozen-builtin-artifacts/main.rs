// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0

use std::path::PathBuf;

fn main() {
    if let Err(err) = run() {
        eprintln!("{err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut out_dir = None;
    let mut styles = Vec::new();
    let mut args = std::env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out-dir" => {
                out_dir = Some(PathBuf::from(
                    args.next().ok_or_else(|| "--out-dir requires a path".to_string())?,
                ));
            }
            "--style" => {
                styles.push(args.next().ok_or_else(|| "--style requires a style".to_string())?);
            }
            "--help" | "-h" => {
                print_help();
                return Ok(());
            }
            _ => return Err(format!("unknown argument: {arg}")),
        }
    }

    let out_dir = out_dir.ok_or_else(|| "--out-dir is required".to_string())?;
    if styles.is_empty() {
        styles.extend(i_slint_compiler::fileaccess::styles().into_iter().map(Into::into));
    }

    let module_path =
        i_slint_compiler::typeloader::TypeLoader::generate_frozen_builtin_artifact_files(
            &styles, &out_dir,
        )?;
    println!("{}", module_path.display());
    Ok(())
}

fn print_help() {
    println!(
        "Usage: slint-frozen-builtin-artifacts --out-dir <dir> [--style <style> ...]\n\
         Generates postcard builtin cache files and frozen_builtin_artifacts.rs.\n\
         If no --style is specified, artifacts are generated for all embedded builtin styles."
    );
}
