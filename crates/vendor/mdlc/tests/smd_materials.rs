#[test]
fn material_lines_preserve_spaces_unicode_and_numeric_prefixes() {
    let source = include_str!("fixtures/cube.smd");
    for name in [
        "02 - Default",
        "Стол основа",
        "123",
        "paint with spaces",
        "02 03",
        "02 a b c d e f g h",
    ] {
        let source = source.replace("\nmaterial\n", &format!("\n{name}\n"));
        let parsed = mdlc::smd::parse_smd(&source).unwrap();
        assert!(!parsed.triangles.is_empty());
        assert!(
            parsed
                .triangles
                .iter()
                .all(|triangle| triangle.material == name)
        );
    }
}

#[test]
fn a_short_first_vertex_is_not_a_numeric_material_name() {
    let source = include_str!("fixtures/cube.smd");
    let index = source.rfind("\nmaterial\n").unwrap() + "\nmaterial\n".len();
    let end = index + source[index..].find('\n').unwrap();
    let mut source = source.to_owned();
    source.replace_range(index..end, "0");
    assert!(mdlc::smd::parse_smd(&source).is_err());
}
