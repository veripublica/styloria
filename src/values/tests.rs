use super::*;

fn check(decl: &str) -> ValueCheck {
    let css: &'static str = Box::leak(decl.to_string().into_boxed_str());
    let items = crate::parse_block_contents(css).0;
    match items.first() {
        Some(crate::BlockItem::Declaration(d)) => check_declaration(&d.node),
        other => panic!("{decl}: {other:?}"),
    }
}

#[track_caller]
fn all(want: ValueCheck, decls: &[&str]) {
    let wrong: Vec<_> = decls
        .iter()
        .map(|d| (d, check(d)))
        .filter(|(_, got)| *got != want)
        .collect();
    assert!(wrong.is_empty(), "want {want:?}, got {wrong:?}");
}

#[test]
fn valid_values() {
    all(
        ValueCheck::Valid,
        &[
            "width: 12px",
            "width: 0",
            "width: 50%",
            "width: auto",
            "width: calc(100% - 2em)",
            "width: fit-content(20em)",
            "color: red",
            "color: #fff",
            "color: #12345678",
            "color: rgb(1 2 3 / 50%)",
            "color: rgb(1, 2, 3)",
            "color: rgba(0,0,0,.5)",
            "color: currentColor",
            "margin: 1px 2px 3px 4px",
            "margin: 0 auto",
            "display: block",
            "display: inline flow-root",
            "display: none",
            "font: 12px serif",
            "font: italic bold 12px/1.5 \"Foo Bar\", Georgia, serif",
            "font-family: \"Times New Roman\", Times, serif",
            "background: url(a.png) no-repeat center / cover, #fff",
            "background: none",
            "background-image: url(\"a.png\")",
            "border: 1px solid black",
            "text-align: justify",
            "line-height: 1.4",
            "transform: rotate(45deg) translateX(10px)",
            "position: fixed",
            "font-variant: small-caps",
            "font-variant: normal",
            "font-variant: common-ligatures small-caps oldstyle-nums",
            "text-decoration: underline dotted red",
            "page-break-before: always",
            "width: inherit",
            "width: REVERT-LAYER",
            "--anything: { whatever ] }",
            "WIDTH: 1PX",
        ],
    );
}

#[test]
fn invalid_values() {
    // epubveri's probes, 2026-10-18: epubcheck is silent on every one.
    all(
        ValueCheck::Invalid,
        &[
            "width: banana",
            "color: 12px",
            "margin: 1px 2px 3px 4px 5px",
            "display: blokc",
            "font-size: -1em",
            "width: 12",
            "color: #12",
            "width: inherit 1px",
            "background: url(a.png) url(b.png)",
            "color: rgb(1, 2 3)",
        ],
    );
}

#[test]
fn unknown_values_say_why() {
    use UnknownValue::*;
    for (decl, why) in [
        ("width: var(--w)", Substitution),
        ("width: calc(var(--w) * 2)", Substitution),
        ("colr: red", UnknownProperty),
        ("-webkit-width: banana", PrefixedProperty),
        ("width: url(a b.png)", BadToken),
        ("width: ", Empty),
        ("display: oeb-page-head", OtherSpecification),
        ("display: OEB-PAGE-FOOT", OtherSpecification),
        ("display: -webkit-box", PrefixedValue),
        ("width: -moz-available", PrefixedValue),
        ("width: some-future-function(1px)", Unread),
        ("font-size: -webkit-calc(1em)", Unread),
    ] {
        assert_eq!(check(decl), ValueCheck::Unknown(why), "{decl}");
    }
    // An `oeb-page-head` anywhere else is just an unknown keyword.
    assert_eq!(check("float: oeb-page-head"), ValueCheck::Invalid);
}

#[test]
fn important_belongs_to_the_declaration() {
    assert_eq!(check("width: 1px !important"), ValueCheck::Valid);
    assert_eq!(check("width: banana !important"), ValueCheck::Invalid);
}

#[test]
fn hostile_values_end_unknown_not_invalid() {
    // A value much too long for the step budget.
    let long = format!("font-family: {}", vec!["a"; 20_000].join(" "));
    let got = check(&long);
    assert!(
        matches!(
            got,
            ValueCheck::Valid | ValueCheck::Unknown(UnknownValue::TooComplex)
        ),
        "{got:?}"
    );
    let deep = format!("width: {}1px{}", "calc(".repeat(60), ")".repeat(60));
    assert_ne!(check(&deep), ValueCheck::Invalid);
}

#[test]
fn every_property_grammar_is_read() {
    let g = grammars();
    let unread: Vec<_> = g
        .properties
        .iter()
        .filter(|(_, n)| n.is_none())
        .map(|(p, _)| *p)
        .collect();
    assert!(unread.is_empty(), "{unread:?}");
}

fn descriptor(at_rule: &str, decl: &str) -> ValueCheck {
    let css: &'static str = Box::leak(decl.to_string().into_boxed_str());
    let items = crate::parse_block_contents(css).0;
    match items.first() {
        Some(crate::BlockItem::Declaration(d)) => {
            check_descriptor(at_rule, &d.node.name, &d.node.value)
        }
        other => panic!("{decl}: {other:?}"),
    }
}

#[test]
fn descriptor_values() {
    use ValueCheck::*;
    for (at, decl, want) in [
        ("font-face", "font-family: \"Foo Bar\"", Valid),
        ("font-face", "font-family: Foo Bar", Valid),
        ("font-face", "font-weight: 100 900", Valid),
        ("font-face", "font-weight: bolder", Invalid),
        ("font-face", "font-style: oblique 10deg 20deg", Valid),
        ("font-face", "font-display: swap", Valid),
        ("font-face", "font-display: fast", Invalid),
        (
            "font-face",
            "unicode-range: U+0000-00FF, U+0131, u+4??",
            Valid,
        ),
        ("font-face", "unicode-range: latin", Invalid),
        ("FONT-FACE", "FONT-STYLE: italic", Valid),
        ("counter-style", "system: fixed 3", Valid),
        ("counter-style", "symbols: \"*\" \"†\"", Valid),
        ("counter-style", "range: 1 10, 20 infinite", Valid),
        ("counter-style", "pad: 3 \"0\"", Valid),
        ("counter-style", "pad: -3 \"0\"", Invalid),
        ("page", "size: A4 landscape", Valid),
        ("page", "size: 10cm", Valid),
        ("page", "size: huge", Invalid),
        ("page", "margin: 1in 2in", Valid),
        ("page", "margin: banana", Invalid),
        ("property", "inherits: false", Valid),
        ("property", "inherits: maybe", Invalid),
        ("font-face", "font-weight: inherit", Valid),
    ] {
        assert_eq!(descriptor(at, decl), want, "@{at} {decl}");
    }
    use UnknownValue::*;
    for (at, decl, why) in [
        ("font-face", "colour: red", UnknownProperty),
        ("font-face", "-webkit-src: x", PrefixedProperty),
        ("media", "width: 1px", UnknownProperty),
        ("font-face", "font-weight: var(--w)", Substitution),
    ] {
        assert_eq!(descriptor(at, decl), Unknown(why), "@{at} {decl}");
    }
}

#[test]
fn font_face_src_is_read_entry_by_entry() {
    use ValueCheck::*;
    for (decl, want) in [
        ("src: url(a.woff2) format(\"woff2\")", Valid),
        (
            "src: url(a.woff2) format(woff2), url(a.ttf) format(\"truetype\")",
            Valid,
        ),
        (
            "src: url(\"a.otf\") format(\"opentype\") tech(variations, color-COLRv1)",
            Valid,
        ),
        ("src: local(\"Foo Bar\"), local(Foo Bar), url(a.ttf)", Valid),
        (
            "src: url(a.eot?#iefix) format(\"embedded-opentype\"), url(a.woff) format(\"woff\", \"x\")",
            Valid,
        ),
        // One good entry is enough: a browser skips the rest.
        ("src: url(a.ttf) format(), url(b.ttf)", Valid),
        ("src: url(a.ttf) format(nope), local(12)", Invalid),
        ("src: 12px", Invalid),
        ("src: \"a.ttf\"", Invalid),
    ] {
        assert_eq!(descriptor("font-face", decl), want, "{decl}");
    }
    // Entry by entry stays linear: 20,000 entries, each read once.
    // Distinct entries, so the verdict cache cannot help.
    let entries: Vec<_> = (0..20_000)
        .map(|i| format!("url(a{i}) format(x y)"))
        .collect();
    let long = format!("src: {}", entries.join(", "));
    let start = std::time::Instant::now();
    assert_eq!(descriptor("font-face", &long), Invalid);
    assert!(start.elapsed().as_secs() < 2, "{:?}", start.elapsed());
}
