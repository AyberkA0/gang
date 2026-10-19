//! Layer collision matrices (`m_LayerCollisionMatrix` in
//! `DynamicsManager.asset` and `Physics2DSettings.asset`; DESIGN.md §14.4).
//!
//! Unity writes the matrix as hex: one 32-bit mask per layer, each as four
//! little-endian bytes. Row `i` bit `j` set means layers `i` and `j`
//! collide.

use unity_yaml::{File, Scalar, Value};

pub const KEY: &str = "m_LayerCollisionMatrix";

/// The rows of a matrix, or `None` when the text is not one.
pub fn parse(hex: &str) -> Option<Vec<u32>> {
    let hex = hex.trim();
    if hex.is_empty() || hex.len() % 8 != 0 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16))
        .collect::<Result<_, _>>()
        .ok()?;
    Some(
        bytes
            .chunks(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    )
}

/// The matrix as Unity writes it.
pub fn format(rows: &[u32]) -> String {
    rows.iter()
        .flat_map(|r| r.to_le_bytes())
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Three-way merge, bit by bit. A cell can only be switched one way, so two
/// people who changed the same cell changed it the same way: there is never
/// a conflict.
pub fn merge(base: &[u32], ours: &[u32], theirs: &[u32]) -> Vec<u32> {
    let len = base.len().max(ours.len()).max(theirs.len());
    let get = |rows: &[u32], i: usize| rows.get(i).copied().unwrap_or(u32::MAX);
    (0..len)
        .map(|i| {
            let b = get(base, i);
            b ^ ((b ^ get(ours, i)) | (b ^ get(theirs, i)))
        })
        .collect()
}

/// Swaps layers `a` and `b`: their rows and their columns.
pub fn swap(rows: &[u32], a: u32, b: u32) -> Vec<u32> {
    let n = rows.len();
    let index = |i: usize| -> usize {
        if i == a as usize {
            b as usize
        } else if i == b as usize {
            a as usize
        } else {
            i
        }
    };
    let mut out = vec![u32::MAX; n];
    for (i, row) in rows.iter().enumerate() {
        let target = index(i);
        if target >= n {
            continue;
        }
        let mut moved = 0u32;
        for j in 0..32usize {
            if row & (1 << j) != 0 {
                moved |= 1 << index(j).min(31);
            }
        }
        out[target] = moved;
    }
    out
}

/// The matrix in a parsed settings file, with its document index.
fn find(file: &File) -> Option<(usize, Vec<u32>)> {
    file.docs.iter().enumerate().find_map(|(i, doc)| {
        let text = doc.body()?.get(KEY)?.text()?;
        Some((i, parse(&text)?))
    })
}

fn set(file: &mut File, doc: usize, rows: &[u32]) {
    if let Some(body) = file.docs[doc].body_mut() {
        body.set(KEY, Value::Scalar(Scalar::raw(format(rows))));
    }
}

/// Before a merge: settles the matrix bit by bit and gives both sides the
/// result, so the field merge has nothing left to ask.
pub fn premerge(base: &File, ours: &mut File, theirs: &mut File) {
    let (Some((_, b)), Some((oi, o)), Some((ti, t))) = (find(base), find(ours), find(theirs))
    else {
        return;
    };
    if o == t {
        return;
    }
    let merged = merge(&b, &o, &t);
    set(ours, oi, &merged);
    set(theirs, ti, &merged);
}

/// Moves a layer from `from` to `to` (a free number) in a settings file's
/// matrix. Returns whether anything changed.
pub fn move_layer(file: &mut File, from: u32, to: u32) -> bool {
    let Some((doc, rows)) = find(file) else {
        return false;
    };
    let moved = swap(&rows, from, to);
    if moved == rows {
        return false;
    }
    set(file, doc, &moved);
    true
}

/// Readable lines for the cells that differ: `Projectile × Projectile: off`.
pub fn describe(old: &[u32], new: &[u32], names: &[String]) -> Vec<String> {
    let name = |i: usize| -> String {
        names
            .get(i)
            .filter(|n| !n.is_empty())
            .cloned()
            .unwrap_or_else(|| format!("layer {i}"))
    };
    let mut lines = Vec::new();
    let n = old.len().max(new.len());
    for i in 0..n {
        for j in i..32usize.min(n.max(i + 1)) {
            let before = old.get(i).is_none_or(|r| r & (1 << j) != 0);
            let after = new.get(i).is_none_or(|r| r & (1 << j) != 0);
            if before != after {
                lines.push(format!(
                    "{} × {}: {}",
                    name(i),
                    name(j),
                    if after {
                        "collide"
                    } else {
                        "ignore each other"
                    }
                ));
            }
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_writes_unity_s_encoding() {
        // Row 8 as the field test wrote it: Projectile ignores Projectile.
        let field = format!("{}fffeffff{}", "ffffffff".repeat(8), "ffffffff".repeat(23));
        let rows = parse(&field).unwrap();
        assert_eq!(rows.len(), 32);
        assert_eq!(rows[8], !(1 << 8));
        assert_eq!(rows[7], u32::MAX);
        assert_eq!(format(&rows), field);
    }

    #[test]
    fn merging_never_conflicts() {
        let base = vec![u32::MAX; 32];
        let mut ours = base.clone();
        ours[8] &= !(1 << 8); // Projectile ignores Projectile
        let mut theirs = base.clone();
        theirs[9] &= !(1 << 9); // Interactable ignores Interactable
        theirs[8] &= !(1 << 8); // the same cell, the same way
        let merged = merge(&base, &ours, &theirs);
        assert_eq!(merged[8], !(1 << 8));
        assert_eq!(merged[9], !(1 << 9));
        assert_eq!(merged[0], u32::MAX);
    }

    #[test]
    fn a_moved_layer_takes_its_row_and_column() {
        let mut rows = vec![u32::MAX; 32];
        rows[8] &= !(1 << 8); // 8 × 8 off
        rows[8] &= !(1 << 3); // 8 × 3 off
        rows[3] &= !(1 << 8);
        let moved = swap(&rows, 8, 9);
        assert_eq!(moved[8], u32::MAX);
        assert_eq!(moved[9], !((1 << 9) | (1 << 3)));
        assert_eq!(moved[3], !(1 << 9));
        let names: Vec<String> = ["Default", "", "", "Water"]
            .iter()
            .map(|s| s.to_string())
            .chain((4..9).map(|_| String::new()))
            .chain(["Interactable".to_owned()])
            .collect();
        let lines = describe(&rows, &moved, &names);
        assert!(
            lines.contains(&"Water × Interactable: ignore each other".to_owned()),
            "{lines:?}"
        );
        assert!(
            lines.contains(&"Water × layer 8: collide".to_owned()),
            "{lines:?}"
        );
    }
}
