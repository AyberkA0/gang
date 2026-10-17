//! Prints where a file's round trip first differs: `cargo run --example roundtrip -- <file>`.
fn main() {
    let path = std::env::args().nth(1).expect("usage: roundtrip <file>");
    let text = std::fs::read_to_string(&path).expect("read");
    let mut file = unity_yaml::parse(&text).expect("parse");
    if std::env::args().nth(2).as_deref() == Some("--reformat") {
        file.forget_raw();
    }
    let out = file.to_text();
    match text.bytes().zip(out.bytes()).position(|(a, b)| a != b) {
        Some(pos) => {
            let line = text[..pos].matches('\n').count() + 1;
            println!("differs at line {line}");
            println!(
                "--- original\n{}",
                &text[pos.saturating_sub(200)..(pos + 200).min(text.len())]
            );
            println!(
                "--- written\n{}",
                &out[pos.saturating_sub(200)..(pos + 200).min(out.len())]
            );
        }
        None if text.len() != out.len() => {
            println!("lengths differ: {} vs {}", text.len(), out.len())
        }
        None => println!("identical"),
    }
}
