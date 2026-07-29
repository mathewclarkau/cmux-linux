//! `cmux theme list` — print available bundled theme presets.

use std::path::Path;

/// List bundled theme presets: prints `<name>  <path>` for each, sorted
/// alphabetically by name.
pub fn run_list() -> i32 {
    let themes_dir = std::env::current_dir().unwrap_or_else(|_| Path::new(".").into());
    let entries = std::fs::read_dir(themes_dir.join("themes")).unwrap_or_else(|_| {
        eprintln!("cmux: themes/ directory not found; is the binary running from the repo root?");
        std::process::exit(1);
    });

    let mut themes: Vec<(String, String)> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let path = e.path();
            let file_name = path.file_stem()?.to_str()?;
            let name = file_name.to_string();
            if path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("toml")) {
                let rel = path
                    .strip_prefix(&themes_dir)
                    .ok()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| path.display().to_string());
                Some((name, rel))
            } else {
                None
            }
        })
        .collect();

    themes.sort_by(|a, b| a.0.cmp(&b.0));

    for (name, path) in themes {
        println!("{name}  {path}");
    }

    0
}
