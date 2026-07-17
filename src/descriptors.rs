//! Per-at-rule descriptor vocabularies, from MDN's css/at-rules.json
//! (https://github.com/mdn/data). Each at-rule below defines its own
//! closed set of descriptor names; `validate` checks an at-rule's
//! declarations against the matching set. Sorted, for binary search.
//! Regenerated from the source, not hand-maintained.

const COUNTER_STYLE: &[&str] = &[
    "additive-symbols",
    "fallback",
    "negative",
    "pad",
    "prefix",
    "range",
    "speak-as",
    "suffix",
    "symbols",
    "system",
];
const FONT_FACE: &[&str] = &[
    "ascent-override",
    "descent-override",
    "font-display",
    "font-family",
    "font-feature-settings",
    "font-stretch",
    "font-style",
    "font-variation-settings",
    "font-weight",
    "line-gap-override",
    "size-adjust",
    "src",
    "unicode-range",
];
const FONT_PALETTE_VALUES: &[&str] = &["base-palette", "font-family", "override-colors"];
const PAGE: &[&str] = &["bleed", "marks", "page-orientation", "size"];
const PROPERTY: &[&str] = &["inherits", "initial-value", "syntax"];
const VIEW_TRANSITION: &[&str] = &["navigation", "types"];

/// The canonical at-rule name and its descriptor set, for a lower-cased
/// at-rule name (without the leading `@`). `None` for at-rules whose
/// body is not a descriptor list (e.g. `@media`, `@font-feature-values`).
pub(crate) fn descriptors_for(at_rule: &str) -> Option<(&'static str, &'static [&'static str])> {
    Some(match at_rule {
        "counter-style" => ("counter-style", COUNTER_STYLE),
        "font-face" => ("font-face", FONT_FACE),
        "font-palette-values" => ("font-palette-values", FONT_PALETTE_VALUES),
        "page" => ("page", PAGE),
        "property" => ("property", PROPERTY),
        "view-transition" => ("view-transition", VIEW_TRANSITION),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::descriptors_for;

    #[test]
    fn every_descriptor_set_is_sorted_deduped_and_lowercase() {
        for at in [
            "counter-style",
            "font-face",
            "font-palette-values",
            "page",
            "property",
            "view-transition",
        ] {
            let (canon, set) = descriptors_for(at).expect("known at-rule");
            assert_eq!(canon, at);
            for pair in set.windows(2) {
                assert!(pair[0] < pair[1], "{at}: not sorted at {:?}", pair[0]);
            }
            for &d in set {
                assert_eq!(d, d.to_ascii_lowercase(), "{at}: not lowercase {d:?}");
            }
        }
    }

    #[test]
    fn non_descriptor_at_rules_are_none() {
        for at in [
            "media",
            "supports",
            "font-feature-values",
            "keyframes",
            "import",
        ] {
            assert!(descriptors_for(at).is_none(), "{at} should be None");
        }
    }
}
