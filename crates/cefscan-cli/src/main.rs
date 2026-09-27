//! `cefscan` 命令行入口。

mod cli;
mod output;

use std::io::Write;
use std::process::ExitCode;

use cefscan_core::scan;
use clap::Parser;

fn main() -> ExitCode {
    let args = cli::Cli::parse();

    let options = args.scan_options();
    let kinds = match args.kind_filter() {
        Ok(kinds) => kinds,
        Err(error) => {
            eprintln!("cefscan: {error}");
            return ExitCode::from(2);
        }
    };
    let min_size = match args.min_size_bytes() {
        Ok(size) => size,
        Err(error) => {
            eprintln!("cefscan: {error}");
            return ExitCode::from(2);
        }
    };

    let outcome = match scan(&options) {
        Ok(outcome) => outcome,
        Err(error) => {
            eprintln!("cefscan: {error}");
            return ExitCode::from(1);
        }
    };

    let mut apps = outcome.apps;
    if !kinds.is_empty() {
        apps.retain(|app| kinds.contains(&app.kind));
    }
    if args.running_only {
        apps.retain(|app| app.running);
    }
    if let Some(min) = min_size {
        apps.retain(|app| app.size >= min);
    }
    if args.ascending {
        apps.reverse();
    }

    let text = match output::render(&apps, args.format) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("cefscan: 序列化失败: {error}");
            return ExitCode::from(1);
        }
    };

    if let Some(path) = &args.output {
        match std::fs::write(path, text) {
            Ok(()) => {}
            Err(error) => {
                eprintln!("cefscan: 写入 {} 失败: {error}", path.display());
                return ExitCode::from(1);
            }
        }
    } else if let Err(error) = std::io::stdout().write_all(text.as_bytes()) {
        eprintln!("cefscan: {error}");
        return ExitCode::from(1);
    }

    if !args.quiet {
        let stats = &outcome.stats;
        eprintln!(
            "cefscan: {} 个应用，占用 {}（列表合计 {}），候选 {} 个，遍历 {} 个目录，后端 {}，耗时 {} ms",
            stats.apps,
            output::human_size(stats.total_bytes),
            output::human_size(stats.sum_bytes),
            stats.candidates,
            stats.dirs_scanned,
            stats.backend,
            stats.elapsed_ms
        );
    }
    if args.verbose {
        eprintln!("cefscan: 后端={} 候选={}", outcome.stats.backend, outcome.stats.candidates);
        for app in apps.iter().take(20) {
            if let Some(evidence) = app.evidence {
                eprintln!("cefscan: {} <- {evidence}", app.path.display());
            }
        }
    }

    ExitCode::SUCCESS
}
