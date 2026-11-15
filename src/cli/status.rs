use serde_json::Value;

pub(super) fn render(data: &Value) -> String {
    let backup = &data["backup"];
    let count = |key: &str| backup[key].as_u64().unwrap_or(0);
    let local = count("local_files");
    let archived = count("backed_up");
    let pending = local.saturating_sub(archived);
    let copying = count("awaiting_local");
    let scanning = count("scanning") > 0;
    let edits = count("pending_edits");
    let mut lines = Vec::new();
    if !backup.is_object() {
        lines.push("Local files: unknown".into());
        lines.push("NAS backup: unknown; use status --json for details".into());
    } else {
        let mut line = format!("Local files: {local} saved");
        if copying > 0 {
            line.push_str(&format!("; {copying} awaiting local copy"));
        }
        if scanning {
            line.push_str("; checking card");
        }
        lines.push(line);
        let mut line = format!("NAS backup: {archived}/{local} local files backed up");
        if pending > 0 {
            line.push_str(&format!("; {pending} waiting"));
        }
        if pending > 0 || copying > 0 {
            if data["archive"]["availability"].as_str() == Some("unavailable") {
                line.push_str(" — NAS unavailable; will retry");
            } else if let Some(reason) =
                (data["capabilities"]["nas_publication"]["allowed"].as_bool() == Some(false))
                    .then(|| data["capabilities"]["nas_publication"]["reason"].as_str())
                    .flatten()
            {
                line.push_str(" — ");
                line.push_str(&friendly_reason(reason));
            }
        }
        lines.push(line);
        if edits > 0 {
            let mut line = format!("Saved edits: {edits} saved edit(s) pending on NAS");
            if data["capabilities"]["nas_metadata_replacement"]["allowed"].as_bool() == Some(false)
                && data["capabilities"]["nas_metadata_replacement"]["reason"]
                    .as_str()
                    .is_some_and(|r| r.contains("exchange"))
            {
                line.push_str(" — NAS cannot safely replace existing edit files");
            } else if let Some(reason) = data["worker"]["publication_reason"].as_str() {
                line.push_str(" — ");
                line.extend(reason.chars().filter(|c| !c.is_control()).take(160));
            }
            lines.push(line);
        }
    }
    let space = &data["space"];
    let available = (|| {
        if space["occupancy_reason"].as_str().is_some() {
            return None;
        }
        let reserved = space["remaining_reservations"].as_u64()?;
        let reserve = space["reserve"].as_u64()?;
        let budget = space["budget"]
            .as_u64()?
            .saturating_sub(space["exclusive_total"].as_u64()?)
            .saturating_sub(reserved);
        let disk = space["filesystem_free"]
            .as_u64()?
            .min(space["state_filesystem_free"].as_u64()?)
            .saturating_sub(reserve)
            .saturating_sub(reserved);
        Some(budget.min(disk))
    })();
    let bytes = |value: Option<u64>| {
        value
            .map(|value| format!("{:.1} GiB", value as f64 / 1073741824.0))
            .unwrap_or_else(|| "unknown".into())
    };
    lines.push(format!(
        "Cache: {} available / {} budget ({} used)",
        bytes(available),
        bytes(space["budget"].as_u64()),
        bytes(space["exclusive_total"].as_u64())
    ));
    let mut attention = Vec::new();
    if count("conflicts") > 0 {
        attention.push(format!(
            "{} conflicts; run photo_sorter conflicts list",
            count("conflicts")
        ));
    }
    if count("physical_issues") > 0 {
        attention.push(format!(
            "{} file verification issues; run photo_sorter verify",
            count("physical_issues")
        ));
    }
    if space["occupancy_reason"].as_str().is_some() {
        attention.push("cache measurement failed; see status --json".into());
    }
    if !attention.is_empty() {
        lines.push(format!("Attention: {}", attention.join("; ")));
    }
    lines.join("\n")
}

fn friendly_reason(reason: &str) -> String {
    if reason.contains("no matching filesystem/version/mode qualification") {
        "uploads paused; archive checks required".into()
    } else if reason.contains("has not been observed") {
        "checking archive availability".into()
    } else if reason.contains("nas_qualification_scratch") {
        "uploads paused; configure the NAS scratch folder once (see docs/usage.md)".into()
    } else if reason.contains("archive checks failed") {
        "uploads paused; NAS checks failed (see status --json)".into()
    } else {
        format!(
            "uploads paused: {}",
            reason
                .chars()
                .filter(|c| !c.is_control())
                .take(120)
                .collect::<String>()
        )
    }
}
