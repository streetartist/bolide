use bolide_parser::parse_source;

#[test]
fn parse_splice_and_macro() {
    for (src, label) in [
        (r#"let a = $x;"#, "splice"),
        (r#"let a = ($x) + 1;"#, "paren splice"),
        (r#"macro twice($x:expr) { quote { ($x) + ($x); } }"#, "macro twice"),
        (r#"macro twice($x:expr) { ($x) + ($x); }"#, "macro no quote"),
        (r#"assert!(true);"#, "assert call"),
        (r#"@derive(Debug) class P { x: int; }"#, "derive"),
        (r#"macro m($x:expr) { print($x); }"#, "macro print"),
    ] {
        match parse_source(src) {
            Ok(p) => println!("OK {}: {} stmts", label, p.statements.len()),
            Err(e) => println!("ERR {}: {}", label, e),
        }
    }
}
