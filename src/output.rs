use anyhow::Result;
use serde_json::Value;
use std::io::{self, IsTerminal, Write};

pub fn print(value: &Value, json: bool) -> Result<()> {
    let mut out = io::stdout().lock();
    if json || !io::stdout().is_terminal() {
        serde_json::to_writer(&mut out, value)?;
        writeln!(out)?;
        return Ok(());
    }
    let array = value.as_array().or_else(|| {
        [
            "sandboxes",
            "volumes",
            "images",
            "ingresses",
            "sets",
            "credentials",
            "connections",
            "apiKeys",
            "members",
            "calls",
            "principals",
            "tools",
        ]
        .iter()
        .find_map(|key| value.get(key).and_then(Value::as_array))
    });
    if let Some(rows) = array {
        if rows.is_empty() {
            writeln!(out, "No results.")?;
            return Ok(());
        }
        let keys: Vec<&str> = [
            "id",
            "name",
            "email",
            "status",
            "role",
            "url",
            "source",
            "registry",
            "userAgent",
            "current",
            "tool",
        ]
        .into_iter()
        .filter(|k| rows.iter().any(|r| !r[*k].is_null()))
        .collect();
        if !keys.is_empty() {
            let cells: Vec<Vec<String>> = rows
                .iter()
                .map(|row| keys.iter().map(|k| cell(&row[*k])).collect())
                .collect();
            let widths: Vec<usize> = keys
                .iter()
                .enumerate()
                .map(|(i, k)| {
                    cells
                        .iter()
                        .map(|r| r[i].chars().count())
                        .max()
                        .unwrap_or(0)
                        .max(k.len())
                })
                .collect();
            writeln!(
                out,
                "{}",
                keys.iter()
                    .enumerate()
                    .map(|(i, k)| format!("{:<width$}", k.to_uppercase(), width = widths[i]))
                    .collect::<Vec<_>>()
                    .join("  ")
            )?;
            for row in cells {
                writeln!(
                    out,
                    "{}",
                    row.iter()
                        .enumerate()
                        .map(|(i, c)| format!("{c:<width$}", width = widths[i]))
                        .collect::<Vec<_>>()
                        .join("  ")
                )?;
            }
            if let Some(cursor) = value["nextCursor"].as_str() {
                writeln!(out, "\nNext page: --cursor {cursor} (or use --all)")?;
            }
            return Ok(());
        }
    }
    serde_json::to_writer_pretty(&mut out, value)?;
    writeln!(out)?;
    Ok(())
}

pub fn safe(text: &str) -> String {
    text.chars()
        .flat_map(|c| {
            if c.is_control() {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

fn cell(value: &Value) -> String {
    if value.is_null() {
        return "—".into();
    }
    let value = value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string());
    let text = safe(&value);
    if text.chars().count() > 80 {
        format!("{}…", text.chars().take(79).collect::<String>())
    } else {
        text
    }
}
