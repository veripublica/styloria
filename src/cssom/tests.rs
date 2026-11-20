use super::*;
use crate::parser::{parse_block_contents, parse_stylesheet};

fn code(k: Kept) -> String {
    match k {
        Kept::Yes => "Y".into(),
        Kept::No { reason, .. } => format!("N:{reason:?}"),
        Kept::Unknown { reason, .. } => format!("U:{reason:?}"),
    }
}

fn verdicts(css: &str) -> Vec<String> {
    let (sheet, _) = parse_stylesheet(css);
    cssom_declarations(&sheet)
        .into_iter()
        .map(|d| code(d.kept))
        .collect()
}

fn check(cases: &[(&str, &[&str])]) {
    let mut failed = Vec::new();
    for (css, want) in cases {
        let got = verdicts(css);
        if got != *want {
            failed.push(format!("{css:?}\n   want {want:?}\n    got {got:?}"));
        }
    }
    assert!(failed.is_empty(), "\n{}", failed.join("\n"));
}

// The six books epubveri rejects (epub-test-books v0.8.0), and their
// neighbours in the same sets. Firefox 160 agrees on every one.
#[test]
fn epub_test_books_direction_sets() {
    check(&[
        ("p { direction: sideways; }", &["N:InvalidValue"]),
        ("@foo { p { direction: rtl; } }", &["N:UnknownAtRule"]),
        ("} p { direction: rtl; }", &["N:InvalidSelector"]),
        ("p:foo { direction: rtl; }", &["N:InvalidSelector"]),
        ("p, :foo { direction: rtl; }", &["N:InvalidSelector"]),
        (
            r#"[epub|type~="footnote"] { direction: rtl; }"#,
            &["N:UndeclaredPrefix"],
        ),
        (
            r#"@namespace epub "http://www.idpf.org/2007/ops"; [epub|type~="footnote"] { direction: rtl }"#,
            &["Y"],
        ),
        (":where(:foo) { direction: rtl; }", &["Y"]),
        (":is(:foo, p) { direction: rtl }", &["Y"]),
        ("<!-- p { direction: rtl } -->", &["Y"]),
        (
            "p { color: \"a\n; direction: rtl }",
            &["N:InvalidValue", "Y"],
        ),
        ("p { direction: rtl", &["Y"]),
        ("P { DIRECTION: RTL }", &["Y"]),
        (
            "p { unicode-bidi: sideways } q { unicode-bidi: isolate }",
            &["N:InvalidValue", "Y"],
        ),
    ]);
}

#[test]
fn kept_is_not_valid() {
    check(&[
        ("p { direction: var(--x) }", &["Y"]),
        ("p { direction: rtl !important }", &["Y"]),
        ("p { color: }", &["N:InvalidValue"]),
        (
            "p { color: red !important !important }",
            &["N:InvalidValue"],
        ),
        (
            "p { background: url(a b); color: red }",
            &["N:InvalidValue", "Y"],
        ),
    ]);
}

#[test]
fn conditions_do_not_drop() {
    check(&[
        (
            "@supports not (display: block) { p { direction: rtl } }",
            &["Y"],
        ),
        ("@supports (foo bar) { p { direction: rtl } }", &["Y"]),
        (
            "@supports foo { p { direction: rtl } }",
            &["N:InvalidSupports"],
        ),
        ("@media not all { p { direction: rtl } }", &["Y"]),
        ("@media (foo: bar) { p { direction: rtl } }", &["Y"]),
        ("@media !!! { p { direction: rtl } }", &["Y"]),
        ("@container (width > 0) { p { color: red } }", &["Y"]),
        ("@container foo { p { color: red } }", &["Y"]),
        ("@container (foo) { p { color: red } }", &["Y"]),
        ("@container style(--x: 1) { p { color: red } }", &["Y"]),
        ("@container !!! { p { color: red } }", &["N:InvalidAtRule"]),
        ("@container { p { color: red } }", &["N:InvalidAtRule"]),
    ]);
}

#[test]
fn group_rules() {
    check(&[
        ("@layer a { p { direction: rtl } }", &["Y"]),
        ("@layer a, b { p { direction: rtl } }", &["N:InvalidLayer"]),
        ("@layer a b { p { direction: rtl } }", &["N:InvalidLayer"]),
        ("@starting-style { p { direction: rtl } }", &["Y"]),
        (
            "@starting-style foo { p { color: red } }",
            &["N:InvalidAtRule"],
        ),
        ("@scope (.a) { direction: rtl }", &["Y"]),
        ("@scope (.a) { p { direction: rtl } }", &["Y"]),
        ("@scope !!! { p { color: red } }", &["N:InvalidAtRule"]),
        ("@scope (:foo) { p { color: red } }", &["N:InvalidAtRule"]),
        ("@scope to (.b) { p { color: red } }", &["Y"]),
        ("@scope { p { color: red } }", &["Y"]),
        ("@scope (.a) { > p { color: red } }", &["Y"]),
        ("@scope (.a) { & p { color: red } }", &["Y"]),
        ("@scope (.a) { @media print { color: red } }", &["Y"]),
        ("@media print { color: red }", &["N:NotAllowedHere"]),
        (
            "@supports (color: red) { color: red }",
            &["N:NotAllowedHere"],
        ),
        ("@layer a { color: red }", &["N:NotAllowedHere"]),
        ("@starting-style { color: red }", &["N:NotAllowedHere"]),
        ("@media print { @font-face { font-family: x } }", &["Y"]),
        ("@supports (color: red) { @page { margin: 0 } }", &["Y"]),
        ("@layer a { @keyframes k { from { color: red } } }", &["Y"]),
        (
            "@media print { @foo { p { color: red } } }",
            &["N:UnknownAtRule"],
        ),
    ]);
}

#[test]
fn nesting() {
    check(&[
        ("p { span { direction: rtl } }", &["Y"]),
        ("p { :foo { direction: rtl } }", &["N:InvalidSelector"]),
        ("p:foo { span { direction: rtl } }", &["N:InvalidSelector"]),
        ("p { @media print { direction: rtl } }", &["Y"]),
        ("p { span { color: red } direction: rtl }", &["Y", "Y"]),
        ("p { @media print { span { color: red } } }", &["Y"]),
        (
            "p { @font-face { font-family: x } color: red }",
            &["N:NotAllowedHere", "Y"],
        ),
        (
            "p { @keyframes k { from { color: red } } }",
            &["N:NotAllowedHere"],
        ),
        ("p { @page { margin: 0 } }", &["N:NotAllowedHere"]),
        ("p { @starting-style { color: red } }", &["Y"]),
        ("p { @layer a { color: red } }", &["Y"]),
        ("p { @scope (.a) { color: red } }", &["Y"]),
        ("p { @container (width > 0) { color: red } }", &["Y"]),
        (
            "p { @foo { color: red } color: blue }",
            &["N:UnknownAtRule", "Y"],
        ),
    ]);
}

#[test]
fn descriptor_rules() {
    check(&[
        (
            "@font-face { font-family: x; direction: rtl }",
            &["Y", "N:NotAllowedHere"],
        ),
        ("@font-face foo { font-family: x }", &["N:InvalidAtRule"]),
        (
            "@font-face { font-family: x !important }",
            &["U:ImportantDescriptor"],
        ),
        (
            "@font-face { p { color: red } font-family: x }",
            &["N:NotAllowedHere", "Y"],
        ),
        (
            "@counter-style x { system: cyclic; symbols: a; direction: rtl }",
            &["Y", "Y", "N:NotAllowedHere"],
        ),
        (
            "@counter-style none { system: cyclic }",
            &["N:InvalidAtRule"],
        ),
        (
            "@property --x { syntax: '*'; inherits: false; direction: rtl }",
            &["Y", "Y", "N:NotAllowedHere"],
        ),
        ("@property x { syntax: '*' }", &["N:InvalidAtRule"]),
        (
            "@view-transition { navigation: auto; direction: rtl }",
            &["Y", "N:NotAllowedHere"],
        ),
        (
            "@view-transition foo { navigation: auto }",
            &["N:InvalidAtRule"],
        ),
        (
            "@font-feature-values Foo { @swash { direction: 1 } }",
            &["U:UnreadAtRule"],
        ),
        ("@swash { direction: 1 }", &["N:NotAllowedHere"]),
    ]);
}

#[test]
fn position_try() {
    check(&[
        (
            "@position-try --x { top: 0; margin: 0; width: 1px; color: red; direction: rtl }",
            &["Y", "Y", "Y", "N:NotAllowedHere", "N:NotAllowedHere"],
        ),
        (
            "@position-try --x { top: 0 !important }",
            &["N:ImportantNotAllowed"],
        ),
        ("@position-try x { top: 0 }", &["N:InvalidAtRule"]),
    ]);
}

// CSS Paged Media 3 §4.3 and Appendix A. Firefox 160 drops `direction` in
// `@page`; the appendix lists it, and the spec decides.
#[test]
fn page() {
    check(&[
        ("@page { direction: rtl }", &["Y"]),
        (
            "@page { @top-center { direction: rtl; unicode-bidi: isolate } }",
            &["Y", "Y"],
        ),
        ("@top-center { direction: rtl }", &["N:NotAllowedHere"]),
        ("@page :foo { direction: rtl }", &["N:InvalidSelector"]),
        ("@page { margin: 1cm; size: A4 }", &["Y", "Y"]),
        ("@page a:left { margin: 1cm }", &["Y"]),
        ("@page :left, :right { margin: 1cm }", &["Y"]),
        ("@page a :left { margin: 1cm }", &["N:InvalidSelector"]),
        ("@page :nth(1) { margin: 1cm }", &["U:PageSelector"]),
        (
            "@page { unicode-bidi: isolate; color: red; display: block }",
            &["N:NotAllowedHere", "Y", "N:NotAllowedHere"],
        ),
        ("@page { box-shadow: none }", &["U:PageUndefined"]),
        ("@page { margin: 1cm !important }", &["Y"]),
        (
            "@page { p { color: red } margin: 1cm }",
            &["N:NotAllowedHere", "Y"],
        ),
    ]);
}

#[test]
fn keyframes() {
    check(&[
        ("@keyframes k { from { direction: rtl } }", &["Y"]),
        (
            "@keyframes k { from { direction: rtl !important } }",
            &["N:ImportantNotAllowed"],
        ),
        (
            "@keyframes k { foo { direction: rtl } }",
            &["N:InvalidSelector"],
        ),
        (
            "@-webkit-keyframes k { from { direction: rtl } }",
            &["U:NonstandardAtRule"],
        ),
        (
            "@-webkit-keyframes k { from { direction: rtl !important } }",
            &["N:ImportantNotAllowed"],
        ),
        (
            "@keyframes none { from { color: red } }",
            &["N:InvalidAtRule"],
        ),
        ("@keyframes 1 { from { color: red } }", &["N:InvalidAtRule"]),
        ("@keyframes { from { color: red } }", &["N:InvalidAtRule"]),
        (
            "@keyframes initial { from { color: red } }",
            &["N:InvalidAtRule"],
        ),
        ("@keyframes \"x\" { from { color: red } }", &["Y"]),
        (
            "@keyframes k { color: red; from { color: blue } }",
            &["N:NotAllowedHere", "Y"],
        ),
        (
            "@keyframes k { @media print { from { color: red } } }",
            &["N:NotAllowedHere"],
        ),
        ("@keyframes k { from, 50% { color: red } }", &["Y"]),
        (
            "@keyframes k { 101% { color: red } }",
            &["N:InvalidSelector"],
        ),
        ("@keyframes k { 0 { color: red } }", &["N:InvalidSelector"]),
        ("@keyframes k { FROM { color: red } }", &["Y"]),
        ("@keyframes k { entry 10% { color: red } }", &["Y"]),
        (
            "@keyframes k { from { animation-duration: 1s; animation-timing-function: linear } }",
            &["U:KeyframeAnimationProperty", "Y"],
        ),
    ]);
}

#[test]
fn at_rules() {
    check(&[
        (
            "@-moz-document url-prefix() { p { direction: rtl } }",
            &["U:NonstandardAtRule"],
        ),
        (
            "@-moz-document url-prefix() { p:foo { direction: rtl } }",
            &["N:InvalidSelector"],
        ),
        (
            "@-webkit-media print { p { color: red } }",
            &["U:NonstandardAtRule"],
        ),
        ("@-foo-bar { p { color: red } }", &["U:NonstandardAtRule"]),
        (
            "@document url-prefix() { p { color: red } }",
            &["N:UnknownAtRule"],
        ),
        (
            "@import url(x.css) { p { color: red } }",
            &["N:InvalidAtRule"],
        ),
        (
            "p { color: red } @import url(x.css); q { direction: rtl }",
            &["Y", "Y"],
        ),
    ]);
}

#[test]
fn selectors_in_doubt() {
    check(&[
        (
            "p::-webkit-scrollbar { direction: rtl }",
            &["U:VendorSelector"],
        ),
        ("p:-moz-focusring { direction: rtl }", &["U:VendorSelector"]),
        (":is(p:-moz-focusring, q) { direction: rtl }", &["Y"]),
        (
            ":not(p:-moz-focusring) { direction: rtl }",
            &["U:VendorSelector"],
        ),
        (
            "p::-webkit-scrollbar:foo { direction: rtl }",
            &["U:VendorSelector"],
        ),
    ]);
}

#[test]
fn outermost_reason_and_span() {
    let css = "@foo { p:bar { direction: sideways } }";
    let (sheet, _) = parse_stylesheet(css);
    let d = cssom_declarations(&sheet);
    let Kept::No { reason, span } = d[0].kept else {
        panic!("{:?}", d[0].kept)
    };
    assert_eq!(reason, DropReason::UnknownAtRule);
    assert_eq!(&css[span.start..span.end], "@foo");

    let css = "@-webkit-keyframes k { 0 { color: red } }";
    let (sheet, _) = parse_stylesheet(css);
    let d = cssom_declarations(&sheet);
    let Kept::No { reason, span } = d[0].kept else {
        panic!("{:?}", d[0].kept)
    };
    assert_eq!(reason, DropReason::InvalidSelector);
    assert_eq!(&css[span.start..span.end], "0");

    let css = "p, q:foo { color: red }";
    let (sheet, _) = parse_stylesheet(css);
    let Kept::No { span, .. } = cssom_declarations(&sheet)[0].kept else {
        panic!()
    };
    assert_eq!(&css[span.start..span.end], "p, q:foo");
}

#[test]
fn contexts() {
    let css = "p { a: b } @keyframes k { from { a: b } } @page { a: b; @top-left { a: b } } \
               @font-face { a: b } @position-try --x { a: b } @font-feature-values F { @swash { a: 1 } }";
    let (sheet, _) = parse_stylesheet(css);
    let got: Vec<_> = cssom_declarations(&sheet)
        .iter()
        .map(|d| d.context)
        .collect();
    assert_eq!(
        got,
        [
            DeclarationContext::Style,
            DeclarationContext::Keyframe,
            DeclarationContext::Page,
            DeclarationContext::PageMargin("top-left"),
            DeclarationContext::Descriptors("font-face"),
            DeclarationContext::PositionTry,
            DeclarationContext::Unread,
        ]
    );
}

#[test]
fn style_attribute() {
    let got = |css| {
        let (items, _) = parse_block_contents(css);
        cssom_block_declarations(&items)
            .into_iter()
            .map(|d| code(d.kept))
            .collect::<Vec<_>>()
    };
    assert_eq!(got("direction: sideways"), ["N:InvalidValue"]);
    assert_eq!(got("direction: rtl"), ["Y"]);
    assert_eq!(got("direction: var(--x)"), ["Y"]);
    assert_eq!(
        got("p { direction: rtl } unicode-bidi: isolate"),
        ["N:NotAllowedHere", "Y"]
    );
    assert_eq!(got("@media print { direction: rtl }"), ["N:NotAllowedHere"]);
}

#[test]
fn every_declaration_once_in_order() {
    let css = "a { b: c; d { e: f } g: h } @media x { i { j: k } } @foo { l: m }";
    let (sheet, _) = parse_stylesheet(css);
    let names: Vec<_> = cssom_declarations(&sheet)
        .iter()
        .map(|d| d.declaration.node.name.to_string())
        .collect();
    assert_eq!(names, ["b", "e", "g", "j", "l"]);
}

// w3c/csswg-drafts#7129: U+2023 is a name code point only before the 2026
// CRD. A selector that needs it is in doubt; a forgiving list that drops it
// either way is not.
#[test]
fn narrowed_idents() {
    check(&[
        ("p.a\u{2023}b { color: red }", &["U:Selector7129"]),
        ("p { .\u{2023} { color: red } }", &["U:Selector7129"]),
        ("p:where(.\u{2023}, .x) { color: red }", &["Y"]),
        ("p:foo.\u{2023} { color: red }", &["N:InvalidSelector"]),
    ]);
}
