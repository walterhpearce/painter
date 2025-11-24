use llvm_ir_analysis::llvm_ir::Module;
use llvm_ir_analysis::ModuleAnalysis;
use rustc_demangle::demangle;
use std::path::Path;
use walkdir::WalkDir;

/// Top error type returned during any stage of analysis from compile to data import.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    ///
    #[error("IO Error: {0}")]
    IoError(#[from] std::io::Error),
    ///
    #[error("LLVM IR failure: {0}")]
    LLVMError(String),
    ///
    #[error("Compilation Failure: {0}")]
    CompileFailed(String),
    ///
    #[error("Clean stage failed")]
    CleanFailure(std::process::Output),
}

const BLOCKED_STRINGS: &[&str] = &["llvm.", "__rust", "rt::", "std::", "core::", "alloc::"];

/// Extract all function calls/invocations within a bytecode file. Returns a `Vec<(String,String)>`
/// of (caller, callee) demangled function names.
///
/// # Panics
/// This function will panic if iterating the `Roots::bytecode_root` fails.
///
/// This function will panic if an LLVM parsing error occurs while parsing the bytecode.
/// # Errors
/// TODO: Failure cases currently panic and should be moved to errors.
#[allow(clippy::unnecessary_wraps)]
pub fn extract_calls<P: AsRef<Path>>(crate_bc_dir: P) -> Result<Vec<(String, String)>, Error> {
    let mut calls = Vec::<(String, String)>::new();

    for bc_entry in std::fs::read_dir(crate_bc_dir.as_ref())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some() && e.path().extension().unwrap() == "bc")
    {
        let bc_path = bc_entry.path();

        let module = Module::from_bc_path(&bc_path)
            .map_err(Error::LLVMError)
            .unwrap();
        let analysis = ModuleAnalysis::new(&module);

        let graph = analysis.call_graph();
        graph.inner().all_edges().for_each(|(src_raw, dst_raw, _)| {
            let src = format!("{:#}", demangle(src_raw));
            let dst = format!("{:#}", demangle(dst_raw));

            if !BLOCKED_STRINGS
                .iter()
                .any(|s| src.contains(*s) || dst.contains(*s))
            {
                calls.push((src, dst));
            }
        });
    }

    Ok(calls)
}

/// Executes a cargo rustc  within the crates sources directory. This is executed within the
/// `Roots::sources_root` directory inside a given crates version folder.
///
/// # Panics
/// This function will panic if executing `cargo` or `rustc` fails due to OS process execution problems.
/// It will not panic on failure of the command itself.
///
/// This function will panic if the stdout or stderr from `rustc` fails to UTF-8 decode.
///
/// # Errors
/// returns an instance of `Error::CompileFailed`, containing the output of stdout and stderr from the
/// execution.
fn compile_crate<PS: AsRef<Path>, PC: AsRef<Path>>(
    name: &str,
    version: &str,
    src_path: PS,
    bc_root: PC,
) -> Result<(), crate::Error> {
    let fullname = format!("{}-{}", &name, version);
    let output_dir = bc_root.as_ref().join(&fullname);

    log::info!("Compiling: {} @ {}", &fullname, output_dir.display());

    // Build the crate with rustc, emitting llvm-bc. We also disable LTO to prevent some inlining
    // to gain better cross-crate function call introspection.
    // TODO: We should further limit optimizations and inlining to get an even better picture.
    let output = std::process::Command::new("cargo")
        .args([
            "+1.67",
            "rustc",
            "--release",
            "--lib",
            "--",
            "-g",
            "--emit=llvm-bc",
            "-C",
            "lto=off",
        ])
        .current_dir(src_path.as_ref())
        .output()
        .unwrap();

    log::trace!("Compiled: {} with result: {:?}", fullname, output);

    if output.status.success() {
        std::fs::create_dir(&output_dir);

        // If the compile succeeded, search for emitted .bc files of bytecode and copy them over
        // to the Roots::bytecode_root directory.
        WalkDir::new(src_path.as_ref())
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().is_some() && e.path().extension().unwrap() == "bc")
            .for_each(|e| {
                let dst = output_dir.join(Path::new(e.path().file_name().unwrap()));
                if dst.exists() {
                    std::fs::remove_file(&dst).unwrap();
                }
                std::fs::copy(e.path(), &dst).unwrap();
            });

        clean(src_path.as_ref())?;
    } else {
        clean(src_path.as_ref())?;

        return Err(Error::CompileFailed(format!(
            "{}\n-----------\n{}",
            std::str::from_utf8(&output.stdout).unwrap(),
            std::str::from_utf8(&output.stderr).unwrap()
        )));
    };

    Ok(())
}

/// Executes a cargo clean within the crates sources directory. This is executed within the
/// `Roots::sources_root` directory inside a given crates version folder.
///
/// # Panics
/// This function will panic if executing `cargo` or `rustc` fails due to OS process execution problems.
/// It will not panic on failure of the command itself.
/// # Errors
/// returns an instance of `Error::CleanFailure`, containing the output of stdout and stderr from the
/// execution.
pub fn clean(path: &Path) -> Result<(), Error> {
    // cargo rustc --release -- -g --emit=llvm-bc
    let output = std::process::Command::new("cargo")
        .arg("+1.60")
        .arg("clean")
        .current_dir(path)
        .output()
        .unwrap();

    std::fs::remove_dir_all(path.join("target"));

    if output.status.success() {
        Ok(())
    } else {
        Err(Error::CleanFailure(output))
    }
}
