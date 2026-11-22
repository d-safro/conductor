//! A small CSV reader (quoted fields, commas and line breaks inside quotes, "" for a quote; comma, semicolon or tab
//! separated, guessed from the first line), and rows looked up by header name.

use std::collections::HashMap;

pub fn rows(text: &str) -> Vec<Vec<String>> {
    let first = text.lines().next().unwrap_or("");
    let sep = [',', ';', '\t'].into_iter().max_by_key(|c| first.matches(*c).count()).unwrap_or(',');
    let mut out = vec![];
    let (mut row, mut field, mut quoted, mut chars) = (vec![], String::new(), false, text.chars().peekable());
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    quoted = false;
                }
            } else {
                field.push(c);
            }
        } else if c == '"' && field.is_empty() {
            quoted = true;
        } else if c == sep {
            row.push(std::mem::take(&mut field));
        } else if c == '\n' || c == '\r' {
            if c == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            row.push(std::mem::take(&mut field));
            if row.iter().any(|f| !f.is_empty()) {
                out.push(std::mem::take(&mut row));
            } else {
                row.clear();
            }
        } else {
            field.push(c);
        }
    }
    row.push(field);
    if row.iter().any(|f| !f.is_empty()) {
        out.push(row);
    }
    out
}

/// A table: the header (lower case, trimmed) and the rows under it.
pub struct Table {
    pub cols: HashMap<String, usize>,
    pub rows: Vec<Vec<String>>,
}

impl Table {
    /// The header is the first row that has one of `markers` (some exports put lines above it).
    pub fn parse(text: &str, markers: &[&str]) -> Option<Table> {
        let all = rows(text);
        let at = all.iter().position(|r| r.iter().any(|c| markers.iter().any(|m| c.trim().eq_ignore_ascii_case(m))))?;
        let cols = all[at].iter().enumerate().map(|(i, c)| (c.trim().to_lowercase(), i)).collect();
        Some(Table { cols, rows: all[at + 1..].to_vec() })
    }
    /// The first of these columns that the table has.
    pub fn col(&self, names: &[&str]) -> Option<usize> {
        names.iter().find_map(|n| self.cols.get(&n.to_lowercase()).copied())
    }
    pub fn get<'a>(&self, row: &'a [String], names: &[&str]) -> &'a str {
        self.col(names).and_then(|i| row.get(i)).map(|s| s.trim()).unwrap_or("")
    }
    pub fn has(&self, names: &[&str]) -> bool {
        self.col(names).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoted_fields() {
        let r = rows("a,b,c\r\n\"x, y\",\"he said \"\"hi\"\"\",\"two\nlines\"\n\n1,,3");
        assert_eq!(r.len(), 3);
        assert_eq!(r[1], ["x, y", "he said \"hi\"", "two\nlines"]);
        assert_eq!(r[2], ["1", "", "3"]);
        assert_eq!(rows("a;b\n1;2")[1], ["1", "2"]);
        let t = Table::parse("Playlist,x\n\nVideo ID,Time Added\nabc,2024", &["video id"]).unwrap();
        assert_eq!(t.get(&t.rows[0], &["Video Id"]), "abc");
    }
}
