use super::*;

#[test]
fn release_ranges() {
    assert_eq!(releases(&["26.3".into()]), "26.3");
    assert_eq!(
        releases(&["26.1".into(), "26.1.1".into(), "26.1.2".into()]),
        "26.1–26.1.2"
    );
    assert_eq!(releases(&[]), "?");
}
