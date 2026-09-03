// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let mut arguments = std::env::args_os().skip(1);
    if arguments.next().as_deref() == Some(std::ffi::OsStr::new("--build-search-qgram")) {
        let Some(source_path) = arguments.next() else {
            eprintln!("missing q-gram source path");
            std::process::exit(2);
        };
        let Some(root) = arguments.next() else {
            eprintln!("missing q-gram root path");
            std::process::exit(2);
        };
        let Some(target_path) = arguments.next() else {
            eprintln!("missing q-gram target path");
            std::process::exit(2);
        };
        if arguments.next().is_some() {
            eprintln!("unexpected q-gram helper argument");
            std::process::exit(2);
        }
        if let Err(error) = beeline_lib::build_search_qgram_sidecar(
            std::path::Path::new(&source_path),
            std::path::Path::new(&root),
            std::path::Path::new(&target_path),
        ) {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }
    beeline_lib::run();
}
