//! C# source helpers: which parts are code (not comments or strings), the
//! layer-number rewrite used by the layer remap (DESIGN.md §14.2), and the
//! type index used by lock levels (DESIGN.md §10.3).

use std::collections::{BTreeMap, BTreeSet};

/// For every byte of `source`, whether it is code (not inside a comment, a
/// string or a character literal).
pub fn code_mask(source: &str) -> Vec<bool> {
    let bytes = source.as_bytes();
    let mut mask = vec![true; bytes.len()];
    let mut i = 0;
    while i < bytes.len() {
        let rest = &bytes[i..];
        let (len, kind) = if rest.starts_with(b"//") {
            (
                rest.iter().position(|b| *b == b'\n').unwrap_or(rest.len()),
                false,
            )
        } else if rest.starts_with(b"/*") {
            (
                rest.windows(2)
                    .skip(2)
                    .position(|w| w == b"*/")
                    .map(|p| p + 4)
                    .unwrap_or(rest.len()),
                false,
            )
        } else if rest.starts_with(b"@\"") || rest.starts_with(b"$@\"") || rest.starts_with(b"@$\"")
        {
            // Verbatim string: "" escapes a quote.
            let start = rest.iter().position(|b| *b == b'"').unwrap() + 1;
            let mut j = start;
            while j < rest.len() {
                if rest[j] == b'"' {
                    if rest.get(j + 1) == Some(&b'"') {
                        j += 2;
                        continue;
                    }
                    break;
                }
                j += 1;
            }
            ((j + 1).min(rest.len()), false)
        } else if rest[0] == b'"' || rest.starts_with(b"$\"") || rest[0] == b'\'' {
            let quote = if rest[0] == b'$' { b'"' } else { rest[0] };
            let start = if rest[0] == b'$' { 2 } else { 1 };
            let mut j = start;
            while j < rest.len() && rest[j] != quote && rest[j] != b'\n' {
                if rest[j] == b'\\' {
                    j += 1;
                }
                j += 1;
            }
            ((j + 1).min(rest.len()), false)
        } else {
            (1, true)
        };
        if !kind {
            for m in mask.iter_mut().skip(i).take(len) {
                *m = false;
            }
        }
        i += len.max(1);
    }
    mask
}

/// The result of rewriting layer numbers in one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerRewrite {
    pub text: String,
    /// 1-based lines that were changed.
    pub changed: Vec<usize>,
    /// Numbers on the given lines that may be the layer but were left as
    /// is. A person should look at them.
    pub review: Vec<Review>,
}

/// A number that may be the moved layer, left as is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    /// 1-based line.
    pub line: usize,
    /// What it is and why it was left: `256 may be a mask for layer 8; left as is`.
    pub message: String,
}

/// Rewrites the layer number `from` to `to` on the given 1-based lines,
/// where it is recognizably a layer or a mask of that one layer:
///
/// - a layer: `.layer = 8`, `.layer == 8`, `case 8:` in a `switch` on a
///   layer, `1 << 8` (also `1u << 8`), an assignment to a name containing
///   "layer" (`const int UI3DLayer = 8;`), a member of an enum whose name
///   contains "layer", the layer arguments of `Physics.IgnoreLayerCollision`
///   and its relatives, and `const` / `static readonly` numbers in the same
///   file whose name is used as a layer (`go.layer = Interact`);
/// - a mask (`256`, `0x100`): assigned to a `LayerMask` or a name containing
///   "mask", the mask argument of a `Physics.*` / `Physics2D.*` call (after a
///   distance, or written in hex), and constants used as one.
///
/// Every other whole number `from` or `1 << from` in code on those lines,
/// and every mask written in hex or used as a mask that includes the layer,
/// is listed in `review`. Strings, comments and preprocessor lines are
/// skipped.
pub fn rewrite_layer_number(
    source: &str,
    lines: &BTreeSet<usize>,
    from: u32,
    to: u32,
) -> LayerRewrite {
    let code = Code::new(source);
    let (from_bit, to_bit) = (1u64 << from, 1u64 << to);
    let mut edits = Vec::new();
    let mut changed = BTreeSet::new();
    let mut review: Vec<Review> = Vec::new();
    let mut flag = |line: usize, message: String| {
        if !review
            .iter()
            .any(|r| r.line == line && r.message == message)
        {
            review.push(Review { line, message });
        }
    };
    for (k, token) in code.tokens.iter().enumerate() {
        if token.kind != Kind::Int || !lines.contains(&token.line) {
            continue;
        }
        let Some(value) = int_value(token.text) else {
            continue;
        };
        let text = token.text;
        let slot = code.slot(k, k + 1);
        let bits = is_bit_literal(text);
        let declared = code.declared_name(k);
        let constant_in = |used: &BTreeSet<&str>| {
            declared.is_some_and(|(name, constant)| constant && used.contains(name))
        };
        let new_value = if value == u64::from(from) {
            if slot == Some(Slot::Layer) || constant_in(&code.used_as_layer) {
                Some(u64::from(to))
            } else {
                flag(
                    token.line,
                    match declared {
                        Some((name, false)) if code.used_as_layer.contains(name) => format!(
                            "{text} is stored in {name}, which is used as a layer but is not a constant; left as is"
                        ),
                        _ => format!("{text} may be layer {from}; left as is"),
                    },
                );
                None
            }
        } else if value == from_bit {
            if slot == Some(Slot::Mask)
                || (bits && matches!(slot, Some(Slot::MaybeMask(_))))
                || constant_in(&code.used_as_mask)
            {
                Some(to_bit)
            } else {
                flag(
                    token.line,
                    match slot {
                        Some(Slot::MaybeMask(call)) => format!(
                            "{text} in {} may be a layer mask or a distance; left as is",
                            code.call_name(call)
                        ),
                        _ => format!("{text} may be a mask for layer {from}; left as is"),
                    },
                );
                None
            }
        } else {
            let masklike = matches!(slot, Some(Slot::Mask | Slot::MaybeMask(_)))
                || bits
                || declared.is_some_and(|(name, _)| code.used_as_mask.contains(name));
            if value & from_bit != 0 && !is_everything(value) && masklike {
                flag(
                    token.line,
                    format!("{text} is a layer mask that includes layer {from}; left as is"),
                );
            }
            None
        };
        if let Some(value) = new_value {
            edits.push((token.start, text.len(), format_like(text, value)));
            changed.insert(token.line);
        }
    }
    for (line, message) in code.implicit_enum_layers(from) {
        if lines.contains(&line) {
            flag(line, message);
        }
    }
    let mut text = String::with_capacity(source.len());
    let mut last = 0;
    for (start, len, new) in &edits {
        text.push_str(&source[last..*start]);
        text.push_str(new);
        last = start + len;
    }
    text.push_str(&source[last..]);
    review.sort_by_key(|r| r.line);
    LayerRewrite {
        text,
        changed: changed.into_iter().collect(),
        review,
    }
}

/// 1-based lines that use a user layer (8–31) by number where a name would
/// do: `go.layer = 8`, `1 << 8`, `LayerMask m = 256`, constants used as
/// layers. For `gang doctor`.
pub fn numeric_layer_uses(source: &str) -> Vec<usize> {
    let code = Code::new(source);
    let mut lines = BTreeSet::new();
    for (k, token) in code.tokens.iter().enumerate() {
        if token.kind != Kind::Int {
            continue;
        }
        let Some(value) = int_value(token.text) else {
            continue;
        };
        let slot = code.slot(k, k + 1);
        let constant = code
            .declared_name(k)
            .filter(|(_, constant)| *constant)
            .map(|(name, _)| name);
        let layer =
            slot == Some(Slot::Layer) || constant.is_some_and(|n| code.used_as_layer.contains(n));
        let mask = slot == Some(Slot::Mask)
            || (is_bit_literal(token.text) && matches!(slot, Some(Slot::MaybeMask(_))))
            || constant.is_some_and(|n| code.used_as_mask.contains(n));
        let user_layer = (8..32).contains(&value);
        let user_mask = value & !0xFF != 0 && !is_everything(value);
        if (layer && user_layer) || (mask && user_mask) {
            lines.insert(token.line);
        }
    }
    lines.into_iter().collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Ident,
    Int,
    Float,
    Str,
    Punct,
}

#[derive(Debug, Clone, Copy)]
struct Token<'a> {
    kind: Kind,
    text: &'a str,
    /// Byte offset in the source.
    start: usize,
    /// 1-based line.
    line: usize,
}

impl Token<'_> {
    fn is(&self, punct: &str) -> bool {
        self.kind == Kind::Punct && self.text == punct
    }

    fn is_word(&self, word: &str) -> bool {
        self.kind == Kind::Ident && self.text == word
    }

    fn is_member_access(&self) -> bool {
        self.is(".") || self.is("?.")
    }
}

const OPERATORS: &[&str] = &[
    "<<=", "??=", "<<", "==", "!=", "<=", ">=", "=>", "&&", "||", "|=", "&=", "^=", "+=", "-=",
    "*=", "/=", "%=", "??", "?.", "::", "++", "--", "->",
];

/// The tokens of the code in `source`: comments and preprocessor lines
/// dropped, each string a single `Str` token.
fn lex(source: &str) -> Vec<Token<'_>> {
    let mask = code_mask(source);
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut line = 1;
    let mut line_start = true;
    let mut i = 0;
    while i < bytes.len() {
        if !mask[i] {
            let end = (i..bytes.len()).find(|&j| mask[j]).unwrap_or(bytes.len());
            let rest = &bytes[i..];
            if !(rest.starts_with(b"//") || rest.starts_with(b"/*")) {
                tokens.push(Token {
                    kind: Kind::Str,
                    text: &source[i..end],
                    start: i,
                    line,
                });
                line_start = false;
            }
            line += bytes[i..end].iter().filter(|b| **b == b'\n').count();
            i = end;
            continue;
        }
        let b = bytes[i];
        if b == b'\n' {
            line += 1;
            line_start = true;
            i += 1;
            continue;
        }
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if b == b'#' && line_start {
            // A preprocessor line (`#if`, `#region 8 …`): not code.
            while i < bytes.len() && !(bytes[i] == b'\n' && mask[i]) {
                if bytes[i] == b'\n' {
                    line += 1;
                }
                i += 1;
            }
            continue;
        }
        line_start = false;
        let rest = &source[i..];
        let c = rest.chars().next().unwrap_or(' ');
        let ident_start = |c: char| c.is_alphabetic() || c == '_';
        let (len, kind) =
            if ident_start(c) || (c == '@' && rest[1..].chars().next().is_some_and(ident_start)) {
                let len = c.len_utf8()
                    + rest[c.len_utf8()..]
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .map(char::len_utf8)
                        .sum::<usize>();
                (len, Kind::Ident)
            } else if b.is_ascii_digit()
                || (b == b'.' && bytes.get(i + 1).is_some_and(u8::is_ascii_digit))
            {
                let (len, float) = number_len(&bytes[i..]);
                (len, if float { Kind::Float } else { Kind::Int })
            } else {
                let len = OPERATORS
                    .iter()
                    .find(|op| rest.starts_with(**op))
                    .map_or(c.len_utf8(), |op| op.len());
                (len, Kind::Punct)
            };
        tokens.push(Token {
            kind,
            text: &source[i..i + len],
            start: i,
            line,
        });
        i += len;
    }
    tokens
}

/// The length of the number literal at the start of `rest`, and whether it
/// is a floating-point one (`8.5`, `8f`, `1e3`).
fn number_len(rest: &[u8]) -> (usize, bool) {
    let digits = |j: &mut usize, ok: fn(&u8) -> bool| {
        while rest.get(*j).is_some_and(|b| ok(b) || *b == b'_') {
            *j += 1;
        }
    };
    let based = rest.len() > 1 && rest[0] == b'0' && matches!(rest[1], b'x' | b'X' | b'b' | b'B');
    let mut j = 0;
    let mut float = false;
    if based {
        j = 2;
        digits(&mut j, u8::is_ascii_hexdigit);
    } else {
        digits(&mut j, u8::is_ascii_digit);
        if rest.get(j) == Some(&b'.') && rest.get(j + 1).is_some_and(u8::is_ascii_digit) {
            float = true;
            j += 1;
            digits(&mut j, u8::is_ascii_digit);
        }
        if matches!(rest.get(j), Some(b'e' | b'E')) {
            let k = j + 1 + usize::from(matches!(rest.get(j + 1), Some(b'+' | b'-')));
            if rest.get(k).is_some_and(u8::is_ascii_digit) {
                float = true;
                j = k;
                digits(&mut j, u8::is_ascii_digit);
            }
        }
    }
    while rest.get(j).is_some_and(u8::is_ascii_alphanumeric) {
        if !based && matches!(rest[j], b'f' | b'F' | b'd' | b'D' | b'm' | b'M') {
            float = true;
        }
        j += 1;
    }
    (j, float)
}

fn int_value(text: &str) -> Option<u64> {
    let body: String = text
        .trim_end_matches(['u', 'U', 'l', 'L'])
        .chars()
        .filter(|c| *c != '_')
        .collect();
    match body.get(..2) {
        Some("0x" | "0X") => u64::from_str_radix(&body[2..], 16).ok(),
        Some("0b" | "0B") => u64::from_str_radix(&body[2..], 2).ok(),
        _ => body.parse().ok(),
    }
}

/// Whether a number is written in hex or binary, the way masks are.
fn is_bit_literal(text: &str) -> bool {
    matches!(text.get(..2), Some("0x" | "0X" | "0b" | "0B"))
}

/// "Everything" masks: they include every layer, moved or not.
fn is_everything(value: u64) -> bool {
    value == u64::from(u32::MAX) || value == u64::MAX || value == i32::MAX as u64
}

/// `value` written like `original`: same base, case, zero padding and suffix.
fn format_like(original: &str, value: u64) -> String {
    let body = original.trim_end_matches(['u', 'U', 'l', 'L']);
    let suffix = &original[body.len()..];
    let (prefix, digits) = if is_bit_literal(body) {
        body.split_at(2)
    } else {
        ("", body)
    };
    let width = if digits.len() > 1 && digits.starts_with('0') {
        digits.chars().filter(|c| *c != '_').count()
    } else {
        0
    };
    let digits = match prefix {
        "0x" | "0X" if digits.chars().any(|c| c.is_ascii_uppercase()) => {
            format!("{value:0width$X}")
        }
        "0x" | "0X" => format!("{value:0width$x}"),
        "0b" | "0B" => format!("{value:0width$b}"),
        _ => value.to_string(),
    };
    format!("{prefix}{digits}{suffix}")
}

/// Where a value goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    /// A layer number.
    Layer,
    /// A layer mask.
    Mask,
    /// An argument of a `Physics.*` call (the method's token) that may be
    /// the layer mask or a distance.
    MaybeMask(usize),
}

/// The words of an identifier, lowercase: `UI3DLayer` → `ui3`, `d`, `layer`.
fn words(name: &str) -> Vec<String> {
    let chars: Vec<char> = name.chars().collect();
    let mut words = Vec::new();
    let mut word = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' || c == '@' {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
            continue;
        }
        let boundary = c.is_uppercase()
            && i > 0
            && (chars[i - 1].is_lowercase()
                || chars[i - 1].is_ascii_digit()
                || (chars[i - 1].is_uppercase()
                    && chars.get(i + 1).is_some_and(|n| n.is_lowercase())));
        if boundary && !word.is_empty() {
            words.push(std::mem::take(&mut word));
        }
        word.extend(c.to_lowercase());
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

/// Whether a name says "layer" (`layer`, `UI3DLayer`, `LAYER_GROUND`, not
/// `player`).
fn is_layer_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    words(name).iter().any(|w| w == "layer" || w == "layers")
        || lower.starts_with("layer")
        || ((lower.ends_with("layer") || lower.ends_with("layers")) && !lower.contains("player"))
}

/// What a name says it holds. Sorting layers, rendering layers and NavMesh
/// areas are other number spaces, and a count of layers is not a layer.
fn name_slot(name: &str) -> Option<Slot> {
    let lower = name.to_ascii_lowercase();
    if ["sorting", "rendering", "area", "navmesh", "count"]
        .iter()
        .any(|w| lower.contains(w))
    {
        None
    } else if lower.contains("mask") {
        Some(Slot::Mask)
    } else if is_layer_name(name) {
        Some(Slot::Layer)
    } else {
        None
    }
}

/// The tokens of one C# file, with what the layer rewrite needs to know
/// about them.
struct Code<'a> {
    tokens: Vec<Token<'a>>,
    /// Per token: the token of the enum's name, when it is directly inside
    /// an enum's braces.
    enums: Vec<Option<usize>>,
    /// Names declared with the type `LayerMask`.
    layer_masks: BTreeSet<&'a str>,
    /// Names whose value is used as a layer (`go.layer = Interact`).
    used_as_layer: BTreeSet<&'a str>,
    /// Names whose value is used as a layer mask.
    used_as_mask: BTreeSet<&'a str>,
}

impl<'a> Code<'a> {
    fn new(source: &'a str) -> Self {
        let tokens = lex(source);
        let mut enums = Vec::with_capacity(tokens.len());
        let mut stack: Vec<Option<usize>> = Vec::new();
        let mut pending = None;
        for (k, t) in tokens.iter().enumerate() {
            enums.push(stack.last().copied().flatten());
            if t.is_word("enum") && tokens.get(k + 1).is_some_and(|n| n.kind == Kind::Ident) {
                pending = Some(k + 1);
            } else if t.is("{") {
                stack.push(pending.take());
            } else if t.is("}") {
                stack.pop();
            } else if t.is(";") {
                pending = None;
            }
        }
        let layer_masks = tokens
            .windows(2)
            .filter(|w| w[0].is_word("LayerMask") && w[1].kind == Kind::Ident)
            .map(|w| w[1].text)
            .collect();
        let mut code = Code {
            tokens,
            enums,
            layer_masks,
            used_as_layer: BTreeSet::new(),
            used_as_mask: BTreeSet::new(),
        };
        let (mut layers, mut masks) = (BTreeSet::new(), BTreeSet::new());
        for k in 0..code.tokens.len() {
            if code.tokens[k].kind != Kind::Ident
                || (k > 0 && code.tokens[k - 1].is_member_access())
            {
                continue;
            }
            let end = code.chain_end(k);
            if code.tokens.get(end).is_some_and(|t| t.is("(")) {
                continue;
            }
            let name = code.tokens[end - 1].text;
            match code.slot(k, end) {
                Some(Slot::Layer) => {
                    layers.insert(name);
                }
                Some(Slot::Mask) => {
                    masks.insert(name);
                }
                _ => {}
            }
        }
        code.used_as_layer = layers;
        code.used_as_mask = masks;
        code
    }

    /// The end (exclusive) of the member chain `a.b.c` starting at `start`.
    fn chain_end(&self, start: usize) -> usize {
        let t = &self.tokens;
        let mut end = start + 1;
        while t.get(end).is_some_and(Token::is_member_access)
            && t.get(end + 1).is_some_and(|n| n.kind == Kind::Ident)
        {
            end += 2;
        }
        end
    }

    /// The names of the member chain ending at token `last`.
    fn chain_before(&self, last: usize) -> Vec<&'a str> {
        let t = &self.tokens;
        if t[last].kind != Kind::Ident {
            return Vec::new();
        }
        let mut start = last;
        while start >= 2 && t[start - 1].is_member_access() && t[start - 2].kind == Kind::Ident {
            start -= 2;
        }
        (start..=last).step_by(2).map(|i| t[i].text).collect()
    }

    /// What a value written to the member chain holds.
    fn classify(&self, chain: &[&str]) -> Option<Slot> {
        let (&last, rest) = chain.split_last()?;
        if self.layer_masks.contains(last) {
            return Some(Slot::Mask);
        }
        if last == "value" {
            if let Some(&owner) = rest.last() {
                if self.layer_masks.contains(owner) || name_slot(owner) == Some(Slot::Mask) {
                    return Some(Slot::Mask);
                }
            }
        }
        name_slot(last)
    }

    /// Where the operand in tokens `start..end` goes.
    fn slot(&self, start: usize, end: usize) -> Option<Slot> {
        self.slot_before(start, end)
            .or_else(|| self.slot_after(end))
    }

    fn slot_before(&self, start: usize, end: usize) -> Option<Slot> {
        let t = &self.tokens;
        let mut j = start;
        // A cast: `(int)Layers.Interact`.
        if j >= 3 && t[j - 1].is(")") && t[j - 2].kind == Kind::Ident && t[j - 3].is("(") {
            j -= 3;
        }
        let prev = t.get(j.checked_sub(1)?)?;
        if prev.is("<<") {
            let one = j >= 2 && t[j - 2].kind == Kind::Int && int_value(t[j - 2].text) == Some(1);
            return one.then_some(Slot::Layer);
        }
        if prev.is_word("case") {
            return self.switch_subject(j - 1);
        }
        let next = t.get(end);
        if next.is_some_and(|n| {
            ["+", "-", "*", "/", "%", "<<", ">"]
                .iter()
                .any(|op| n.is(op))
        }) {
            return None;
        }
        let ends_argument = next.is_some_and(|n| n.is(",") || n.is(")"));
        if prev.kind != Kind::Punct {
            return None;
        }
        match prev.text {
            "=" | "==" | "!=" | "|=" | "&=" | "^=" if j >= 2 => {
                if prev.is("=") && j >= 3 && (t[j - 3].is("{") || t[j - 3].is(",")) {
                    // An enum member: `Interactable = 8` in `enum Layers`.
                    if let Some(name) = self.enums[j - 2] {
                        if let Some(slot) = name_slot(t[name].text) {
                            return Some(slot);
                        }
                    }
                }
                let slot = self.classify(&self.chain_before(j - 2))?;
                let compound = prev.text.len() == 2 && !prev.is("==") && !prev.is("!=");
                (slot == Slot::Mask || !compound).then_some(slot)
            }
            "(" | "," if ends_argument => self.argument(j),
            // A named argument: `layerMask: 256`.
            ":" if ends_argument
                && j >= 3
                && t[j - 2].kind == Kind::Ident
                && (t[j - 3].is("(") || t[j - 3].is(",")) =>
            {
                name_slot(t[j - 2].text)
            }
            _ => None,
        }
    }

    /// `8 == go.layer`.
    fn slot_after(&self, end: usize) -> Option<Slot> {
        let t = &self.tokens;
        let op = t.get(end)?;
        if !(op.is("==") || op.is("!=")) || t.get(end + 1)?.kind != Kind::Ident {
            return None;
        }
        let chain_end = self.chain_end(end + 1);
        if t.get(chain_end).is_some_and(|n| n.is("(")) {
            return None;
        }
        self.classify(&self.chain_before(chain_end - 1))
    }

    /// For `case` at token `case`: what the `switch` it belongs to is on.
    fn switch_subject(&self, case: usize) -> Option<Slot> {
        let t = &self.tokens;
        let mut depth = 0;
        let mut open = case;
        loop {
            open = open.checked_sub(1)?;
            if t[open].is("}") {
                depth += 1;
            } else if t[open].is("{") {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
        }
        let close = open.checked_sub(1)?;
        if !t[close].is(")") {
            return None;
        }
        let mut depth = 0;
        let mut paren = close;
        loop {
            if t[paren].is(")") {
                depth += 1;
            } else if t[paren].is("(") {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            paren = paren.checked_sub(1)?;
        }
        if paren == 0 || !t[paren - 1].is_word("switch") {
            return None;
        }
        let subject = paren + 1;
        if t.get(subject)?.kind != Kind::Ident || self.chain_end(subject) != close {
            return None;
        }
        self.classify(&self.chain_before(close - 1))
    }

    /// The operand at `j` is a whole argument of a call (`t[j - 1]` is `(`
    /// or `,`): which parameter of a known Unity method it is.
    fn argument(&self, j: usize) -> Option<Slot> {
        let t = &self.tokens;
        let mut depth = 0;
        let mut commas = Vec::new();
        let mut k = j - 1;
        loop {
            let token = &t[k];
            if token.kind == Kind::Punct {
                match token.text {
                    ")" | "]" | "}" => depth += 1,
                    "(" | "[" | "{" => {
                        if depth == 0 {
                            if token.text != "(" {
                                return None;
                            }
                            break;
                        }
                        depth -= 1;
                    }
                    "," if depth == 0 => commas.push(k),
                    ";" => return None,
                    _ => {}
                }
            }
            k = k.checked_sub(1)?;
        }
        let open = k;
        if open < 3
            || t[open - 1].kind != Kind::Ident
            || !t[open - 2].is(".")
            || t[open - 3].kind != Kind::Ident
        {
            return None;
        }
        let (class, method) = (t[open - 3].text, t[open - 1].text);
        let index = commas.len();
        let physics = class == "Physics" || class == "Physics2D";
        match method {
            "IgnoreLayerCollision" | "GetIgnoreLayerCollision" if physics => {
                (index < 2).then_some(Slot::Layer)
            }
            "GetLayerCollisionMask" if class == "Physics2D" => (index == 0).then_some(Slot::Layer),
            "SetLayerCollisionMask" if class == "Physics2D" => match index {
                0 => Some(Slot::Layer),
                1 => Some(Slot::Mask),
                _ => None,
            },
            "LayerToName" if class == "LayerMask" => (index == 0).then_some(Slot::Layer),
            _ if physics && index > 0 => {
                let previous_start = commas.get(1).copied().unwrap_or(open) + 1;
                let previous = &t[previous_start..commas[0]];
                Some(if is_distance(previous) {
                    Slot::Mask
                } else {
                    Slot::MaybeMask(open - 1)
                })
            }
            _ => None,
        }
    }

    /// `Physics.Raycast` for the method token of a call.
    fn call_name(&self, method: usize) -> String {
        let t = &self.tokens;
        if method >= 2 && t[method - 1].is(".") {
            format!("{}.{}", t[method - 2].text, t[method].text)
        } else {
            t[method].text.to_owned()
        }
    }

    /// For a number at `k` assigned to a name (`Interact = 8;`): the name,
    /// and whether it is a constant (`const`, `static readonly`, an enum
    /// member).
    fn declared_name(&self, k: usize) -> Option<(&'a str, bool)> {
        let t = &self.tokens;
        if k < 2 || !t[k - 1].is("=") || t[k - 2].kind != Kind::Ident {
            return None;
        }
        let next = t.get(k + 1)?;
        if !(next.is(";") || next.is(",") || next.is("}")) {
            return None;
        }
        let name = t[k - 2].text;
        if self.enums[k - 2].is_some() && k >= 3 && (t[k - 3].is("{") || t[k - 3].is(",")) {
            return Some((name, true));
        }
        let (mut constant, mut is_static, mut readonly) = (false, false, false);
        for token in t[..k - 2].iter().rev() {
            if token.kind == Kind::Punct && matches!(token.text, ";" | "{" | "}" | "(" | ")") {
                break;
            }
            constant |= token.is_word("const");
            is_static |= token.is_word("static");
            readonly |= token.is_word("readonly");
        }
        Some((name, constant || (is_static && readonly)))
    }

    /// Members of enums named for layers that are `layer` by their position
    /// (`enum Layers { Default, …, Interactable }`), with a review message.
    fn implicit_enum_layers(&self, layer: u32) -> Vec<(usize, String)> {
        let t = &self.tokens;
        let mut found = Vec::new();
        let mut current = None;
        let mut next: Option<u64> = None;
        for k in 1..t.len() {
            let Some(e) = self.enums[k] else { continue };
            if name_slot(t[e].text) != Some(Slot::Layer)
                || t[k].kind != Kind::Ident
                || !(t[k - 1].is("{") || t[k - 1].is(","))
            {
                continue;
            }
            if current != Some(e) {
                current = Some(e);
                next = Some(0);
            }
            if t.get(k + 1).is_some_and(|n| n.is("=")) {
                next = match (t.get(k + 2), t.get(k + 3)) {
                    (Some(v), Some(end)) if v.kind == Kind::Int && (end.is(",") || end.is("}")) => {
                        int_value(v.text).map(|v| v + 1)
                    }
                    _ => None,
                };
            } else {
                if next == Some(u64::from(layer)) {
                    found.push((
                        t[k].line,
                        format!(
                            "{} in enum {} is layer {layer} by its position; left as is",
                            t[k].text, t[e].text
                        ),
                    ));
                }
                next = next.map(|v| v + 1);
            }
        }
        found
    }
}

/// Whether an argument looks like a distance: a float literal,
/// `Mathf.Infinity`, `maxDistance`.
fn is_distance(argument: &[Token]) -> bool {
    argument.last().is_some_and(|last| {
        let lower = last.text.to_ascii_lowercase();
        last.kind == Kind::Float
            || (last.kind == Kind::Ident
                && [
                    "infinity", "maxvalue", "distance", "radius", "range", "length",
                ]
                .iter()
                .any(|w| lower.contains(w)))
    })
}

/// Types each script declares, and the type names it mentions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TypeIndex {
    /// Script path → types it declares.
    pub declares: BTreeMap<String, BTreeSet<String>>,
    /// Script path → identifiers it uses.
    pub mentions: BTreeMap<String, BTreeSet<String>>,
}

impl TypeIndex {
    pub fn add(&mut self, path: &str, source: &str) {
        let (declared, mentioned) = scan_types(source);
        self.declares.insert(path.to_owned(), declared);
        self.mentions.insert(path.to_owned(), mentioned);
    }

    /// Scripts that declare a type `path` mentions, and scripts that mention a
    /// type `path` declares. One hop, both directions.
    pub fn neighbors(&self, path: &str) -> BTreeSet<String> {
        let mut result = BTreeSet::new();
        let empty = BTreeSet::new();
        let mine = self.declares.get(path).unwrap_or(&empty);
        let used = self.mentions.get(path).unwrap_or(&empty);
        for (other, declared) in &self.declares {
            if other != path && declared.iter().any(|t| used.contains(t)) {
                result.insert(other.clone());
            }
        }
        for (other, mentioned) in &self.mentions {
            if other != path && mine.iter().any(|t| mentioned.contains(t)) {
                result.insert(other.clone());
            }
        }
        result
    }

    /// Whether the script at `user` mentions a type declared by `script`.
    pub fn uses(&self, user: &str, script: &str) -> bool {
        let (Some(mentioned), Some(declared)) =
            (self.mentions.get(user), self.declares.get(script))
        else {
            return false;
        };
        user != script && declared.iter().any(|t| mentioned.contains(t))
    }
}

const DECLARING: &[&str] = &["class", "struct", "interface", "enum", "record"];

/// Declared type names, and every identifier used in code.
pub fn scan_types(source: &str) -> (BTreeSet<String>, BTreeSet<String>) {
    let mask = code_mask(source);
    let code: String = source
        .char_indices()
        .map(|(i, c)| {
            if mask.get(i).copied().unwrap_or(true) {
                c
            } else {
                ' '
            }
        })
        .collect();
    let mut declared = BTreeSet::new();
    let mut mentioned = BTreeSet::new();
    let mut previous = String::new();
    let mut word = String::new();
    for c in code.chars().chain(std::iter::once(' ')) {
        if c.is_alphanumeric() || c == '_' {
            word.push(c);
            continue;
        }
        if !word.is_empty() {
            if !word.chars().next().unwrap().is_ascii_digit() {
                if DECLARING.contains(&previous.as_str()) {
                    declared.insert(word.clone());
                } else {
                    mentioned.insert(word.clone());
                }
            }
            previous = std::mem::take(&mut word);
        }
    }
    for name in &declared {
        mentioned.remove(name);
    }
    (declared, mentioned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_comments_and_strings() {
        let source = "int a = 8; // 8 in a comment\nvar s = \"8\"; /* 8 */ var t = @\"a\"\"8\";\n";
        let mask = code_mask(source);
        let code: String = source
            .char_indices()
            .filter(|(i, _)| mask[*i])
            .map(|(_, c)| c)
            .collect();
        assert_eq!(code.matches('8').count(), 1);
    }

    fn remap(source: &str) -> LayerRewrite {
        let all: BTreeSet<usize> = (1..=source.lines().count()).collect();
        rewrite_layer_number(source, &all, 8, 9)
    }

    fn review_lines(result: &LayerRewrite) -> Vec<usize> {
        result.review.iter().map(|r| r.line).collect()
    }

    #[test]
    fn rewrites_recognized_layer_numbers_on_given_lines() {
        let source = "\
gameObject.layer = 8;
if (other.layer == 8) {}
int mask = 1 << 8;
const int UI3DLayer = 8;
SetLayerRecursively(go, 8);
var count = 8;
int mask2 = 1 << 18;
// layer 8 in a comment
";
        let result = remap(source);
        assert_eq!(result.changed, vec![1, 2, 3, 4]);
        assert_eq!(review_lines(&result), vec![5, 6]);
        assert!(result.text.starts_with("gameObject.layer = 9;\nif (other.layer == 9) {}\nint mask = 1 << 9;\nconst int UI3DLayer = 9;\nSetLayerRecursively(go, 8);\nvar count = 8;\nint mask2 = 1 << 18;"));

        let only_first: BTreeSet<usize> = [1].into();
        let result = rewrite_layer_number(source, &only_first, 8, 9);
        assert_eq!(result.changed, vec![1]);
        assert!(result.review.is_empty());
        assert!(result.text.contains("other.layer == 8"));
    }

    #[test]
    fn rewrites_single_layer_masks_and_physics_arguments() {
        let source = "\
LayerMask hits = 256;
int interactMask = 0x100;
uint bits = 1u << 8;
camera.cullingMask = 0x0100;
hits.value = 256;
Physics.Raycast(ray, out hit, 100f, 256);
Physics2D.OverlapCircle(p, radius, 0x100);
Physics.Raycast(ray, out hit, Mathf.Infinity, 256, QueryTriggerInteraction.Ignore);
Physics.SphereCast(ray, 1f, out hit, maxDistance: 5f, layerMask: 256);
Physics.IgnoreLayerCollision(8, 2, true);
if (Physics.GetIgnoreLayerCollision(0, 8)) {}
Physics2D.SetLayerCollisionMask(8, 256);
";
        let result = remap(source);
        assert_eq!(result.changed, (1..=12).collect::<Vec<_>>());
        assert!(result.review.is_empty(), "{:?}", result.review);
        assert_eq!(
            result.text,
            "\
LayerMask hits = 512;
int interactMask = 0x200;
uint bits = 1u << 9;
camera.cullingMask = 0x0200;
hits.value = 512;
Physics.Raycast(ray, out hit, 100f, 512);
Physics2D.OverlapCircle(p, radius, 0x200);
Physics.Raycast(ray, out hit, Mathf.Infinity, 512, QueryTriggerInteraction.Ignore);
Physics.SphereCast(ray, 1f, out hit, maxDistance: 5f, layerMask: 512);
Physics.IgnoreLayerCollision(9, 2, true);
if (Physics.GetIgnoreLayerCollision(0, 9)) {}
Physics2D.SetLayerCollisionMask(9, 512);
"
        );
    }

    #[test]
    fn rewrites_layer_enums_and_constants_used_as_layers() {
        let source = "\
enum Layers
{
    Default = 0,
    Interactable = 8,
}
enum Weapons { Sword = 8 }
class A
{
    const int Interact = 8;
    static readonly int Hits = 256;
    const int Unused = 8;
    void F(GameObject go, Ray r)
    {
        go.layer = Interact;
        Physics.Raycast(r, out var h, 10f, Hits);
        int local = 8;
        go.layer = local;
        switch (go.layer) { case 8: break; }
        if (8 == go.layer) {}
        go.layer = (int)Layers.Interactable;
    }
}
";
        let result = remap(source);
        assert_eq!(result.changed, vec![4, 9, 10, 18, 19]);
        assert_eq!(review_lines(&result), vec![6, 11, 16]);
        assert_eq!(
            result.review[2].message,
            "8 is stored in local, which is used as a layer but is not a constant; left as is"
        );
        for expected in [
            "    Interactable = 9,",
            "enum Weapons { Sword = 8 }",
            "const int Interact = 9;",
            "static readonly int Hits = 512;",
            "const int Unused = 8;",
            "int local = 8;",
            "case 9: break;",
            "if (9 == go.layer)",
        ] {
            assert!(
                result.text.contains(expected),
                "{expected}\n{}",
                result.text
            );
        }
    }

    #[test]
    fn lists_every_other_occurrence_for_review() {
        let source = "\
int flags = 0x300;
LayerMask both = 257;
go.layer = 8 + offset;
var size = 256;
items[8] = null;
Physics.Raycast(ray, out hit, 256);
const int MaxPlayers = 8;
enum CollisionLayers { Default, TransparentFX, IgnoreRaycast, Unused3, Water, UI, Unused6, Unused7, Interactable }
uint all = 0xFFFFFFFF; LayerMask none = 0;
";
        let result = remap(source);
        assert!(result.changed.is_empty());
        assert_eq!(result.text, source);
        let messages: Vec<String> = result
            .review
            .iter()
            .map(|r| format!("{}: {}", r.line, r.message))
            .collect();
        assert_eq!(
            messages,
            vec![
                "1: 0x300 is a layer mask that includes layer 8; left as is",
                "2: 257 is a layer mask that includes layer 8; left as is",
                "3: 8 may be layer 8; left as is",
                "4: 256 may be a mask for layer 8; left as is",
                "5: 8 may be layer 8; left as is",
                "6: 256 in Physics.Raycast may be a layer mask or a distance; left as is",
                "7: 8 may be layer 8; left as is",
                "8: Interactable in enum CollisionLayers is layer 8 by its position; left as is",
            ]
        );
    }

    #[test]
    fn leaves_strings_comments_floats_and_names_alone() {
        let source = "\
var name = \"8\"; var s = $\"layer {8} {1 << 8}\"; var v = @\"8\";
// go.layer = 8;
/* int mask = 1 << 8; */
int a = LayerMask.NameToLayer(\"Interact\");
int m = LayerMask.GetMask(\"Interact\", \"Pickup\");
float speed = 8f; double d = 8.5; float e = 8e0f;
int other = 18; int big = 1080; int player = 18;
#region 8
#endregion
";
        let result = remap(source);
        assert_eq!(result.text, source);
        assert!(result.changed.is_empty());
        assert!(result.review.is_empty(), "{:?}", result.review);
    }

    #[test]
    fn numeric_layer_uses_for_doctor() {
        let source = "\
go.layer = 8;
go.layer = 0;
LayerMask m = 256;
int a = LayerMask.NameToLayer(\"A\");
const int Ground = 9;
if (go.layer == Ground) {}
int mask = LayerMask.GetMask(\"A\") | 1;
var count = 8;
";
        assert_eq!(numeric_layer_uses(source), vec![1, 3, 5]);
    }

    #[test]
    fn literals_and_names() {
        assert_eq!(format_like("0x100", 512), "0x200");
        assert_eq!(format_like("0x00000100", 512), "0x00000200");
        assert_eq!(format_like("0xFFu", 0x1FF), "0x1FFu");
        assert_eq!(format_like("0b1_0000_0000", 512), "0b1000000000");
        assert_eq!(format_like("256u", 512), "512u");
        assert_eq!(int_value("1_000"), Some(1000));
        assert_eq!(int_value("0b1_0000_0000"), Some(256));
        assert_eq!(int_value("8UL"), Some(8));
        assert_eq!(words("UI3DLayer"), vec!["ui3", "d", "layer"]);
        assert!(is_layer_name("UI3DLayer"));
        assert!(is_layer_name("LAYER_GROUND"));
        assert!(is_layer_name("groundlayer"));
        assert!(!is_layer_name("MaxPlayers"));
        assert_eq!(name_slot("layerMask"), Some(Slot::Mask));
        assert_eq!(name_slot("sortingLayerID"), None);
        assert_eq!(name_slot("areaMask"), None);
    }

    #[test]
    fn type_index() {
        let mut index = TypeIndex::default();
        index.add(
            "Player.cs",
            "public class Player : MonoBehaviour { PlayerHealth health; }",
        );
        index.add(
            "PlayerHealth.cs",
            "public class PlayerHealth : MonoBehaviour { public event System.Action onDamaged; }",
        );
        index.add(
            "Enemy.cs",
            "class Enemy { void Hit(PlayerHealth h) { /* Player */ } string s = \"Player\"; }",
        );
        index.add("Bullet.cs", "class Bullet {}");
        assert_eq!(
            index.neighbors("PlayerHealth.cs"),
            BTreeSet::from(["Enemy.cs".to_owned(), "Player.cs".to_owned()])
        );
        assert_eq!(
            index.neighbors("Player.cs"),
            BTreeSet::from(["PlayerHealth.cs".to_owned()])
        );
        assert!(index.uses("Enemy.cs", "PlayerHealth.cs"));
        assert!(
            !index.uses("Enemy.cs", "Player.cs"),
            "mentions in comments and strings do not count"
        );
        assert!(index.neighbors("Bullet.cs").is_empty());
    }
}
