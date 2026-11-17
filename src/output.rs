use anyhow::Result;
use serde_json::Value;
use std::io::{self, IsTerminal, Write};

/// Terminals get tables unless --json is set; pipes always get JSON.
pub fn tables(json: bool) -> bool {
    !json && io::stdout().is_terminal()
}

pub fn print(value: &Value, json: bool) -> Result<()> {
    let mut out = io::stdout().lock();
    if !tables(json) {
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
            table(&mut out, &keys, rows)?;
            if let Some(cursor) = value["nextCursor"].as_str() {
                writeln!(
                    out,
                    "\nNext page: rerun with --cursor {cursor} (or use --all)"
                )?;
            }
            return Ok(());
        }
    }
    serde_json::to_writer_pretty(&mut out, value)?;
    writeln!(out)?;
    Ok(())
}

/// Prints one object: indented JSON in a terminal, compact JSON when piped.
/// Unlike `print`, it never shows a field such as `connections` as a table.
pub fn detail(value: &Value, json: bool) -> Result<()> {
    let mut out = io::stdout().lock();
    if tables(json) {
        serde_json::to_writer_pretty(&mut out, value)?;
    } else {
        serde_json::to_writer(&mut out, value)?;
    }
    writeln!(out)?;
    Ok(())
}

/// Writes `keys` of each row as left-aligned columns under uppercase headings.
/// Cells are sanitized and cut at 80 characters; missing values show as —.
pub fn table(out: &mut impl Write, keys: &[&str], rows: &[Value]) -> io::Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn render(keys: &[&str], rows: &[Value]) -> String {
        let mut out = Vec::new();
        table(&mut out, keys, rows).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn table_pads_columns_to_the_widest_cell_or_heading() {
        let rows = [
            json!({"id":"a","name":"short"}),
            json!({"id":"abc","name":"x"}),
        ];
        assert_eq!(
            render(&["id", "name"], &rows),
            "ID   NAME \na    short\nabc  x    \n"
        );
    }

    #[test]
    fn table_marks_missing_values_and_cuts_long_cells() {
        let long = "y".repeat(81);
        let rendered = render(&["id", "name"], &[json!({"id":null,"name":long})]);
        let row = rendered.lines().nth(1).unwrap();
        assert!(row.starts_with("—   "));
        assert!(row.ends_with(&format!("{}…", "y".repeat(79))));
    }

    #[test]
    fn table_escapes_control_characters() {
        let rendered = render(&["name"], &[json!({"name":"a\u{1b}[2Jb"})]);
        assert_eq!(rendered.lines().nth(1), Some("a\\u{1b}[2Jb"));
    }
}
