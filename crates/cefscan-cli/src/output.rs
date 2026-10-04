//! 结果输出：table / json / ndjson / csv / toml。

use std::fmt::Write as _;

use cefscan_core::AppInfo;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Format {
    /// 人类可读表格
    Table,
    /// JSON 数组
    Json,
    /// 每行一个 JSON 对象，适合流式消费
    Ndjson,
    /// CSV（带表头）
    Csv,
    /// TOML
    Toml,
}

pub fn render(apps: &[AppInfo], format: Format) -> Result<String, String> {
    match format {
        Format::Table => Ok(render_table(apps)),
        Format::Json => serde_json::to_string_pretty(apps).map_err(|e| e.to_string()),
        Format::Ndjson => {
            let mut out = String::new();
            for app in apps {
                out.push_str(&serde_json::to_string(app).map_err(|e| e.to_string())?);
                out.push('\n');
            }
            Ok(out)
        }
        Format::Csv => render_csv(apps),
        Format::Toml => {
            #[derive(serde::Serialize)]
            struct Wrapper<'a> {
                apps: &'a [AppInfo],
            }
            toml::to_string_pretty(&Wrapper { apps }).map_err(|e| e.to_string())
        }
    }
}

fn render_csv(apps: &[AppInfo]) -> Result<String, String> {
    let mut writer = csv::Writer::from_writer(Vec::new());
    writer
        .write_record(["path", "root", "kind", "size", "running", "evidence"])
        .map_err(|e| e.to_string())?;
    for app in apps {
        writer
            .write_record([
                app.path.to_string_lossy().as_ref(),
                app.root.to_string_lossy().as_ref(),
                app.kind.label(),
                &app.size.to_string(),
                if app.running { "true" } else { "false" },
                app.evidence.unwrap_or(""),
            ])
            .map_err(|e| e.to_string())?;
    }
    writer.flush().map_err(|e| e.to_string())?;
    let bytes = writer.into_inner().map_err(|e| e.to_string())?;
    String::from_utf8(bytes).map_err(|e| e.to_string())
}

fn render_table(apps: &[AppInfo]) -> String {
    if apps.is_empty() {
        return "没有找到 Chromium 内核应用。\n".to_string();
    }

    let kind_width = apps
        .iter()
        .map(|app| app.kind.label().len())
        .max()
        .unwrap_or(7)
        .max(7);
    let size_width = apps
        .iter()
        .map(|app| human_size(app.size).len())
        .max()
        .unwrap_or(5)
        .max(5);

    let mut out = String::new();
    let _ = writeln!(
        out,
        "{:<kind_width$}  {:>size_width$}  {:<3}  PATH",
        "KIND", "SIZE", "RUN"
    );
    for app in apps {
        let _ = writeln!(
            out,
            "{:<kind_width$}  {:>size_width$}  {:<3}  {}",
            app.kind.label(),
            human_size(app.size),
            if app.running { "*" } else { "" },
            app.path.to_string_lossy()
        );
    }
    out
}

/// 把字节数格式化成 1024 进制的人类可读大小。
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut index = 0;
    while value >= 1024.0 && index < UNITS.len() - 1 {
        value /= 1024.0;
        index += 1;
    }
    if index == 0 {
        format!("{} {}", bytes, UNITS[0])
    } else {
        format!("{value:.1} {}", UNITS[index])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cefscan_core::AppKind;

    fn sample() -> Vec<AppInfo> {
        vec![AppInfo {
            path: std::path::PathBuf::from("C:\\apps\\demo\\app.exe"),
            root: std::path::PathBuf::from("C:\\apps\\demo"),
            kind: AppKind::Electron,
            size: 1536,
            running: true,
            evidence: Some("third_party/electron_node"),
        }]
    }

    #[test]
    fn human_sizes_use_binary_units() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1536), "1.5 KiB");
        assert_eq!(human_size(5 * 1024 * 1024 * 1024), "5.0 GiB");
    }

    #[test]
    fn json_output_is_parseable() {
        let text = render(&sample(), Format::Json).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed[0]["kind"], "electron");
        assert_eq!(parsed[0]["size"], 1536);
        assert_eq!(parsed[0]["running"], true);
    }

    #[test]
    fn ndjson_emits_one_object_per_line() {
        let text = render(&sample(), Format::Ndjson).unwrap();
        assert_eq!(text.lines().count(), 1);
        let parsed: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(parsed["kind"], "electron");
    }

    #[test]
    fn csv_has_a_header_and_escapes_nothing_unexpected() {
        let text = render(&sample(), Format::Csv).unwrap();
        let first = text.lines().next().unwrap();
        assert!(first.starts_with("path,root,kind,size,running,evidence"));
        assert!(text.contains("electron"));
    }

    #[test]
    fn toml_output_round_trips() {
        let text = render(&sample(), Format::Toml).unwrap();
        assert!(text.contains("[[apps]]"));
        let parsed: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(parsed["apps"][0]["kind"].as_str(), Some("electron"));
    }

    #[test]
    fn empty_results_still_render() {
        let text = render(&[], Format::Table).unwrap();
        assert!(text.contains("没有找到"));
        assert_eq!(render(&[], Format::Json).unwrap(), "[]");
    }
}
