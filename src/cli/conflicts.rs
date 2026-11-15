use serde_json::Value;
use std::collections::BTreeMap;

fn text(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(400)
        .collect()
}

pub(super) fn render(data: &Value) -> String {
    let conflicts = data["conflicts"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    if conflicts.is_empty() {
        return "No conflicts need review.".into();
    }
    let display = data["display"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut groups: BTreeMap<String, Vec<(&Value, Option<&Value>)>> = BTreeMap::new();
    for conflict in conflicts {
        let detail = display.iter().find(|d| d["id"] == conflict["id"]);
        let key = detail
            .and_then(|d| d["path"].as_str())
            .or_else(|| conflict["id"].as_str())
            .unwrap_or("Unidentified file");
        groups
            .entry(key.into())
            .or_default()
            .push((conflict, detail));
    }
    let mut lines = vec![format!(
        "{} file(s) need attention ({} report(s))",
        groups.len(),
        conflicts.len()
    )];
    for entries in groups.values() {
        let detail = entries[0].1;
        lines.push(String::new());
        lines.push(text(
            detail
                .and_then(|d| d["path"].as_str())
                .unwrap_or("File not identified"),
        ));
        let mut messages = std::collections::BTreeSet::new();
        for (conflict, detail) in entries {
            let summary = detail
                .and_then(|d| d["summary"].as_str())
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    format!(
                        "{}; filename details unavailable",
                        conflict["kind"]
                            .as_str()
                            .unwrap_or("File mismatch")
                            .replace('_', " ")
                    )
                });
            if messages.insert(summary.clone()) {
                lines.push(format!("  {}", text(&summary)));
            }
            if let Some(action) = detail.and_then(|d| d["action"].as_str())
                && messages.insert(action.into())
            {
                lines.push(format!("  {}", text(action)));
            }
        }
        if let Some(detail) = detail {
            for (key, label) in [("local_path", "Local"), ("archive_path", "NAS")] {
                if let Some(path) = detail[key].as_str() {
                    lines.push(format!("  {label}: {}", text(path)));
                }
            }
        }
        let ids = entries
            .iter()
            .filter_map(|(c, _)| c["id"].as_str())
            .map(text)
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!("  Report IDs: {ids}"));
    }
    if data["has_more"].as_bool() == Some(true)
        && let Some(next) = data["next"].as_str()
    {
        lines.push(format!(
            "\nMore reports: photo_sorter conflicts list --after {}",
            text(next)
        ));
    }
    lines.push("\nFull records: photo_sorter conflicts list --json".into());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_for_one_file_are_grouped_and_terminal_controls_are_escaped() {
        let data = serde_json::json!({"conflicts":[{"id":"one","kind":"nas_projection"},{"id":"two","kind":"nas_projection"}],"display":[
            {"id":"one","path":"a.ORF.rrdata","summary":"NAS is behind\u{001b}[31m","action":"No version choice needed","local_path":"/local/a.ORF.rrdata","archive_path":"/nas/a.ORF.rrdata"},
            {"id":"two","path":"a.ORF.rrdata","summary":"NAS is behind\u{001b}[31m","action":"No version choice needed"}],"has_more":true,"next":"two"});
        let output = render(&data);
        assert!(output.starts_with("1 file(s) need attention (2 report(s))"));
        assert_eq!(output.matches("NAS is behind").count(), 1);
        assert!(output.contains("Local: /local/a.ORF.rrdata"));
        assert!(output.contains("--after two"));
        assert!(!output.contains('\u{001b}'));
    }
}
