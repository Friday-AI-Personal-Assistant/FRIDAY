use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const MAX_FILES: usize = 25;
const MAX_FILE_BYTES: usize = 128 * 1024;
const MAX_TOTAL_BYTES: usize = 180 * 1024;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisFile {
    relative_path: String,
    language: String,
    content: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSymbol {
    name: String,
    kind: String,
    file_path: String,
    line: usize,
    references: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectImport {
    file_path: String,
    import_text: String,
    matched_project_file: Option<String>,
    line: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDiagnostic {
    severity: String,
    code: String,
    message: String,
    file_path: String,
    line: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectIntelligence {
    project_map: Vec<String>,
    entry_points: Vec<String>,
    dependencies: Vec<String>,
    imports: Vec<ProjectImport>,
    symbols: Vec<ProjectSymbol>,
    diagnostics: Vec<ProjectDiagnostic>,
    files_analyzed: usize,
    bytes_analyzed: usize,
    limitations: Vec<String>,
}

fn valid_relative_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 512
        && !path.starts_with('/')
        && !path.starts_with('\\')
        && !path.split(['/', '\\']).any(|part| part == ".." || part.is_empty())
        && !path.contains(':')
}

fn is_entry_point(path: &str, content: &str, language: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    match language {
        "Rust" => content.contains("fn main(") || content.contains("fn main ()"),
        "Python" => name == "main.py" || content.contains("if __name__ == \"__main__\""),
        "Go" => content.lines().any(|line| line.trim() == "func main() {"),
        "Java" => content.contains("public static void main("),
        "C/C++" => content.contains(" main(") || content.contains(" main ("),
        "TypeScript" | "JavaScript" => matches!(name, "main.ts" | "main.tsx" | "index.ts" | "index.tsx" | "main.js" | "index.js" | "app.tsx" | "app.jsx"),
        _ => matches!(name, "main" | "index.html" | "app.py"),
    }
}

fn classify_symbol(line: &str, language: &str) -> Option<(String, String)> {
    let t = line.trim();
    let patterns: &[(&str, &str)] = match language {
        "Rust" => &[("fn ", "function"), ("pub fn ", "function"), ("struct ", "struct"), ("pub struct ", "struct"), ("enum ", "enum"), ("pub enum ", "enum"), ("trait ", "trait"), ("pub trait ", "trait"), ("mod ", "module")],
        "Python" => &[("def ", "function"), ("async def ", "function"), ("class ", "class")],
        "TypeScript" | "JavaScript" => &[("function ", "function"), ("export function ", "function"), ("class ", "class"), ("export class ", "class"), ("interface ", "interface"), ("export interface ", "interface"), ("type ", "type"), ("export type ", "type"), ("const ", "constant"), ("export const ", "constant")],
        "Go" => &[("func ", "function"), ("type ", "type"), ("var ", "variable"), ("const ", "constant")],
        "Java" | "C#" | "Kotlin" => &[("class ", "class"), ("interface ", "interface"), ("enum ", "enum"), ("public ", "member"), ("private ", "member"), ("protected ", "member")],
        _ => &[("function ", "function"), ("class ", "class")],
    };
    for (prefix, kind) in patterns {
        if let Some(rest) = t.strip_prefix(prefix) {
            let name = rest.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
                .next().unwrap_or("");
            if !name.is_empty() && name.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$') {
                return Some((name.to_string(), (*kind).to_string()));
            }
        }
    }
    None
}

fn import_text(line: &str, language: &str) -> Option<String> {
    let t = line.trim();
    let is_import = match language {
        "Rust" => t.starts_with("use ") || t.starts_with("mod "),
        "Python" => t.starts_with("import ") || t.starts_with("from "),
        "TypeScript" | "JavaScript" => t.starts_with("import ") || t.contains("require("),
        "Go" => t.starts_with("import ") || t.starts_with("\"") || t.starts_with("("),
        "Java" | "Kotlin" => t.starts_with("import "),
        "C/C++" => t.starts_with("#include"),
        "C#" => t.starts_with("using "),
        "PHP" => t.starts_with("use "),
        _ => false,
    };
    if is_import { Some(t.chars().take(500).collect()) } else { None }
}

fn manifest_dependencies(path: &str, content: &str, output: &mut BTreeSet<String>) {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    if name == "package.json" {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(content) {
            for section in ["dependencies", "devDependencies", "peerDependencies", "optionalDependencies"] {
                if let Some(map) = json.get(section).and_then(|v| v.as_object()) {
                    for key in map.keys() { output.insert(key.clone()); }
                }
            }
        }
    } else if name == "requirements.txt" {
        for line in content.lines() {
            let item = line.split(['=', '<', '>', '[', ';']).next().unwrap_or("").trim();
            if !item.is_empty() && !item.starts_with('#') { output.insert(item.to_string()); }
        }
    } else if name == "cargo.toml" || name == "go.mod" || name == "pyproject.toml" {
        let mut in_dependencies = false;
        for line in content.lines() {
            let t = line.trim();
            if t.starts_with('[') {
                in_dependencies = t.to_ascii_lowercase().contains("dependenc");
                continue;
            }
            if in_dependencies && !t.is_empty() && !t.starts_with('#') {
                let dep = t.split(['=', ' ', '\t']).next().unwrap_or("").trim_matches('"');
                if !dep.is_empty() { output.insert(dep.to_string()); }
            }
            if name == "go.mod" && t.starts_with("require ") {
                let dep = t.trim_start_matches("require ").split_whitespace().next().unwrap_or("");
                if !dep.is_empty() { output.insert(dep.to_string()); }
            }
        }
    }
}

#[tauri::command]
pub async fn analyze_project_intelligence(files: Vec<AnalysisFile>) -> Result<ProjectIntelligence, String> {
    if files.is_empty() || files.len() > MAX_FILES {
        return Err("Project intelligence requires between 1 and 25 selected files.".into());
    }
    let mut seen = BTreeSet::new();
    let mut total = 0usize;
    for file in &files {
        if !valid_relative_path(&file.relative_path) || !seen.insert(file.relative_path.to_ascii_lowercase()) {
            return Err("The selected project file paths are invalid or duplicated. Select the files again.".into());
        }
        let size = file.content.len();
        if size == 0 || size > MAX_FILE_BYTES {
            return Err("A selected project file exceeds the 128 KB per-file analysis limit.".into());
        }
        total = total.saturating_add(size);
        if total > MAX_TOTAL_BYTES {
            return Err("Selected project content exceeds the 180 KB analysis limit.".into());
        }
    }

    let known_paths: Vec<String> = files.iter().map(|f| f.relative_path.clone()).collect();
    let mut project_map = Vec::new();
    let mut entry_points = Vec::new();
    let mut dependencies = BTreeSet::new();
    let mut imports = Vec::new();
    let mut raw_symbols: Vec<(String, String, String, usize)> = Vec::new();
    let mut diagnostics = Vec::new();

    for file in &files {
        project_map.push(format!("{} ({}, {} bytes)", file.relative_path, file.language, file.content.len()));
        if is_entry_point(&file.relative_path, &file.content, &file.language) {
            entry_points.push(file.relative_path.clone());
        }
        manifest_dependencies(&file.relative_path, &file.content, &mut dependencies);

        for (index, line) in file.content.lines().enumerate() {
            let line_number = index + 1;
            if let Some(text) = import_text(line, &file.language) {
                let lower = text.to_ascii_lowercase();
                let matched = known_paths.iter().find(|candidate| {
                    let stem = candidate.rsplit('/').next().unwrap_or(candidate)
                        .rsplit_once('.').map(|(s, _)| s).unwrap_or(candidate).to_ascii_lowercase();
                    !stem.is_empty() && lower.contains(&stem)
                }).cloned();
                imports.push(ProjectImport {
                    file_path: file.relative_path.clone(),
                    import_text: text,
                    matched_project_file: matched,
                    line: line_number,
                });
            }
            if let Some((name, kind)) = classify_symbol(line, &file.language) {
                raw_symbols.push((name, kind, file.relative_path.clone(), line_number));
            }
            let trimmed = line.trim();
            if trimmed.contains("TODO") || trimmed.contains("FIXME") || trimmed.contains("XXX") {
                diagnostics.push(ProjectDiagnostic {
                    severity: "info".into(),
                    code: "ANNOTATION".into(),
                    message: "Source annotation found; review whether this work remains outstanding.".into(),
                    file_path: file.relative_path.clone(),
                    line: line_number,
                });
            }
            if file.language == "Rust" && (trimmed.contains(".unwrap()") || trimmed.contains(".expect(")) {
                diagnostics.push(ProjectDiagnostic {
                    severity: "warning".into(),
                    code: "RUST_PANIC_PATH".into(),
                    message: "Potential panic path detected (.unwrap/.expect); verify whether this is appropriate for the surrounding error-handling context.".into(),
                    file_path: file.relative_path.clone(),
                    line: line_number,
                });
            }
            if (file.language == "JavaScript" || file.language == "TypeScript") && trimmed.contains("eval(") {
                diagnostics.push(ProjectDiagnostic {
                    severity: "warning".into(),
                    code: "DYNAMIC_EVAL".into(),
                    message: "Dynamic eval call detected; review input trust and whether a safer alternative exists.".into(),
                    file_path: file.relative_path.clone(),
                    line: line_number,
                });
            }
        }
    }

    let mut symbols = Vec::new();
    for (name, kind, file_path, line) in raw_symbols {
        let references = files.iter().map(|f| f.content.matches(&name).count()).sum::<usize>().saturating_sub(1);
        symbols.push(ProjectSymbol { name, kind, file_path, line, references });
    }
    let mut limitations = vec![
        "This is bounded, heuristic source analysis, not a compiler, language server or complete call graph.".to_string(),
        "Only explicitly selected files are considered; imports outside that selection cannot be resolved.".to_string(),
        "Dependency extraction supports common manifest formats and may omit conditional or generated dependencies.".to_string(),
        "Diagnostics are review hints, not proof of a defect; confirm each finding in context.".to_string(),
    ];
    if entry_points.is_empty() {
        limitations.push("No recognised entry point was found in the selected files; include the application manifest or likely startup file.".into());
    }
    Ok(ProjectIntelligence {
        project_map,
        entry_points,
        dependencies: dependencies.into_iter().take(200).collect(),
        imports,
        symbols: symbols.into_iter().take(500).collect(),
        diagnostics: diagnostics.into_iter().take(300).collect(),
        files_analyzed: files.len(),
        bytes_analyzed: total,
        limitations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, language: &str, content: &str) -> AnalysisFile {
        AnalysisFile { relative_path: path.into(), language: language.into(), content: content.into() }
    }

    #[tokio::test]
    async fn maps_entry_points_symbols_imports_and_cross_file_links() {
        let result = analyze_project_intelligence(vec![
            file("src/main.rs", "Rust", "mod helpers;\nfn main() { helpers::run(); }\n"),
            file("src/helpers.rs", "Rust", "pub fn run() {}\n"),
        ]).await.unwrap();
        assert_eq!(result.entry_points, vec!["src/main.rs"]);
        assert!(result.symbols.iter().any(|s| s.name == "run" && s.references > 0));
        assert!(result.imports.iter().any(|i| i.matched_project_file.as_deref() == Some("src/helpers.rs")));
    }

    #[tokio::test]
    async fn extracts_package_json_dependencies() {
        let result = analyze_project_intelligence(vec![file("package.json", "JSON", r#"{"dependencies":{"react":"^19"},"devDependencies":{"vite":"^6"}}"#)]).await.unwrap();
        assert!(result.dependencies.contains(&"react".to_string()));
        assert!(result.dependencies.contains(&"vite".to_string()));
    }

    #[tokio::test]
    async fn flags_are_review_hints_not_errors() {
        let result = analyze_project_intelligence(vec![file("src/lib.rs", "Rust", "fn work() { value.unwrap(); } // TODO review\n")]).await.unwrap();
        assert!(result.diagnostics.iter().any(|d| d.code == "RUST_PANIC_PATH"));
        assert!(result.diagnostics.iter().any(|d| d.code == "ANNOTATION"));
        assert!(result.limitations.iter().any(|s| s.contains("heuristic")));
    }

    #[tokio::test]
    async fn rejects_unsafe_paths_duplicate_paths_and_oversized_input() {
        assert!(analyze_project_intelligence(vec![file("../secret.rs", "Rust", "fn main() {}")]).await.is_err());
        assert!(analyze_project_intelligence(vec![file("src/a.rs", "Rust", "fn a() {}"), file("src/a.rs", "Rust", "fn b() {}")]).await.is_err());
        assert!(analyze_project_intelligence(vec![file("src/a.rs", "Rust", &"x".repeat(MAX_FILE_BYTES + 1))]).await.is_err());
    }
}
