use bolide_parser::parse_source;

#[test]
fn lowercase_none_is_not_a_value_literal() {
    assert!(parse_source("let value: int = none;").is_err());
}

#[test]
fn lowercase_none_is_not_a_pattern() {
    let source = r#"
        let value: Option<int> = Option.None();
        match value {
            none => {},
        }
    "#;
    assert!(parse_source(source).is_err());
}

#[test]
fn option_none_remains_valid() {
    let source = r#"
        fn missing() -> Option<int> {
            return Option.None();
        }
    "#;
    assert!(parse_source(source).is_ok());
}
