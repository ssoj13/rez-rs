use rustpython_sre_engine::{Request, State};

// CPython bytecode for re.compile("ab(cd)") (INFO prefix_len=4, prefix_skip=2).
const CODE: &[u32] = &[
    14, 14, 1, 4, 4, 4, 2, 97, 98, 99, 100, 0, 0, 0, 0, 16, 97, 16, 98, 17, 0, 16, 99, 16, 100, 17,
    1, 1,
];

fn check<S: rustpython_sre_engine::StrDrive>(subject: S, end: usize) {
    let request = Request::new(subject, 0, end, CODE, false);
    let mut state = State::default();
    assert!(state.search(request));
    assert_eq!(state.start, 1);
    assert_eq!(state.cursor.position, 5);
    let (start, end) = state.marks.get(0);
    assert_eq!(start.into_option(), Some(3));
    assert_eq!(end.into_option(), Some(5));
}

#[test]
fn captured_literal_prefix_str() {
    check("xabcdcd", 7);
}

#[test]
fn captured_literal_prefix_bytes() {
    check(b"xabcdcd".as_slice(), 7);
}
