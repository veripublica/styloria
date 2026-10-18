//! Property knowledge the cascade needs: which longhands a shorthand sets,
//! and which physical properties a logical one can stand for.
//!
//! Both answers are about *names*, never values. The shorthand table is
//! generated from MDN's data (see `shorthands.rs`); the logical table follows
//! CSS Logical Properties 1, where which physical side `margin-inline-start`
//! is depends on `writing-mode` and `direction` — so here it is every side it
//! could be.

use std::cmp::Ordering;

use crate::shorthands::SHORTHANDS;

/// The longhands a shorthand property sets, sorted; `None` for a longhand or
/// a name this crate does not know as a shorthand. Nested shorthands are
/// expanded (`border` gives `border-top-width`, not `border-width`), and
/// longhands a shorthand only resets are included (`border` resets
/// `border-image-*`). Compared ASCII case-insensitively.
///
/// ```
/// let longhands = styloria::longhands("padding").unwrap();
/// assert_eq!(longhands, ["padding-bottom", "padding-left", "padding-right", "padding-top"]);
/// assert!(styloria::longhands("padding-top").is_none());
/// ```
pub fn longhands(name: &str) -> Option<&'static [&'static str]> {
    SHORTHANDS
        .binary_search_by(|(k, _)| cmp_ignore_ascii_case(k, name))
        .ok()
        .map(|i| SHORTHANDS[i].1)
}

/// The properties on the other side of the logical/physical divide that
/// `longhand` may set or be set by, depending on the writing mode: for a
/// logical property every physical one it can map to (`inline-size` →
/// `width`, `height`), and for a physical property every logical one that can
/// map to it. Empty when the name is in neither table. `longhand` is
/// lowercase.
pub(crate) fn logical_counterparts(longhand: &str) -> Vec<&'static str> {
    if let Some((_, physical)) = LOGICAL.iter().find(|(l, _)| *l == longhand) {
        return physical.to_vec();
    }
    LOGICAL
        .iter()
        .filter(|(_, physical)| physical.contains(&longhand))
        .map(|(l, _)| *l)
        .collect()
}

fn cmp_ignore_ascii_case(a: &str, b: &str) -> Ordering {
    a.bytes()
        .map(|c| c.to_ascii_lowercase())
        .cmp(b.bytes().map(|c| c.to_ascii_lowercase()))
}

const SIDES: &[&str] = &["top", "right", "bottom", "left"];
const MARGIN: &[&str] = &["margin-top", "margin-right", "margin-bottom", "margin-left"];
const PADDING: &[&str] = &[
    "padding-top",
    "padding-right",
    "padding-bottom",
    "padding-left",
];
const SCROLL_MARGIN: &[&str] = &[
    "scroll-margin-top",
    "scroll-margin-right",
    "scroll-margin-bottom",
    "scroll-margin-left",
];
const SCROLL_PADDING: &[&str] = &[
    "scroll-padding-top",
    "scroll-padding-right",
    "scroll-padding-bottom",
    "scroll-padding-left",
];
const BORDER_WIDTH: &[&str] = &[
    "border-top-width",
    "border-right-width",
    "border-bottom-width",
    "border-left-width",
];
const BORDER_STYLE: &[&str] = &[
    "border-top-style",
    "border-right-style",
    "border-bottom-style",
    "border-left-style",
];
const BORDER_COLOR: &[&str] = &[
    "border-top-color",
    "border-right-color",
    "border-bottom-color",
    "border-left-color",
];
const RADIUS: &[&str] = &[
    "border-top-left-radius",
    "border-top-right-radius",
    "border-bottom-right-radius",
    "border-bottom-left-radius",
];
const SIZE: &[&str] = &["width", "height"];
const MIN_SIZE: &[&str] = &["min-width", "min-height"];
const MAX_SIZE: &[&str] = &["max-width", "max-height"];

/// Each logical longhand (CSS Logical Properties 1, plus the logical
/// longhands of other modules), and every physical longhand it can map to.
const LOGICAL: &[(&str, &[&str])] = &[
    ("block-size", SIZE),
    ("inline-size", SIZE),
    ("min-block-size", MIN_SIZE),
    ("min-inline-size", MIN_SIZE),
    ("max-block-size", MAX_SIZE),
    ("max-inline-size", MAX_SIZE),
    ("margin-block-start", MARGIN),
    ("margin-block-end", MARGIN),
    ("margin-inline-start", MARGIN),
    ("margin-inline-end", MARGIN),
    ("padding-block-start", PADDING),
    ("padding-block-end", PADDING),
    ("padding-inline-start", PADDING),
    ("padding-inline-end", PADDING),
    ("inset-block-start", SIDES),
    ("inset-block-end", SIDES),
    ("inset-inline-start", SIDES),
    ("inset-inline-end", SIDES),
    ("scroll-margin-block-start", SCROLL_MARGIN),
    ("scroll-margin-block-end", SCROLL_MARGIN),
    ("scroll-margin-inline-start", SCROLL_MARGIN),
    ("scroll-margin-inline-end", SCROLL_MARGIN),
    ("scroll-padding-block-start", SCROLL_PADDING),
    ("scroll-padding-block-end", SCROLL_PADDING),
    ("scroll-padding-inline-start", SCROLL_PADDING),
    ("scroll-padding-inline-end", SCROLL_PADDING),
    ("border-block-start-width", BORDER_WIDTH),
    ("border-block-end-width", BORDER_WIDTH),
    ("border-inline-start-width", BORDER_WIDTH),
    ("border-inline-end-width", BORDER_WIDTH),
    ("border-block-start-style", BORDER_STYLE),
    ("border-block-end-style", BORDER_STYLE),
    ("border-inline-start-style", BORDER_STYLE),
    ("border-inline-end-style", BORDER_STYLE),
    ("border-block-start-color", BORDER_COLOR),
    ("border-block-end-color", BORDER_COLOR),
    ("border-inline-start-color", BORDER_COLOR),
    ("border-inline-end-color", BORDER_COLOR),
    ("border-start-start-radius", RADIUS),
    ("border-start-end-radius", RADIUS),
    ("border-end-start-radius", RADIUS),
    ("border-end-end-radius", RADIUS),
    ("overflow-block", &["overflow-x", "overflow-y"]),
    ("overflow-inline", &["overflow-x", "overflow-y"]),
    (
        "overscroll-behavior-block",
        &["overscroll-behavior-x", "overscroll-behavior-y"],
    ),
    (
        "overscroll-behavior-inline",
        &["overscroll-behavior-x", "overscroll-behavior-y"],
    ),
    (
        "contain-intrinsic-block-size",
        &["contain-intrinsic-width", "contain-intrinsic-height"],
    ),
    (
        "contain-intrinsic-inline-size",
        &["contain-intrinsic-width", "contain-intrinsic-height"],
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_sorted_for_binary_search() {
        assert!(SHORTHANDS.windows(2).all(|w| w[0].0 < w[1].0));
        assert!(
            SHORTHANDS
                .iter()
                .all(|(_, l)| l.windows(2).all(|w| w[0] < w[1]))
        );
    }

    #[test]
    fn shorthands_expand_to_longhands() {
        let border = longhands("border").unwrap();
        assert!(border.contains(&"border-top-width"));
        assert!(border.contains(&"border-left-style"));
        // Reset-only: `border` resets `border-image`, which MDN omits.
        assert!(border.contains(&"border-image-source"));
        // Nested shorthands are gone.
        assert!(!border.contains(&"border-width"));
        let font = longhands("FONT").unwrap();
        assert!(font.contains(&"font-size") && font.contains(&"line-height"));
        assert!(font.contains(&"font-variant-caps"));
        assert!(!font.contains(&"font-variant"));
        assert_eq!(longhands("word-wrap").unwrap(), ["overflow-wrap"]);
        assert!(longhands("width").is_none());
        assert!(
            longhands("all").is_none(),
            "`all` is handled by the cascade"
        );
    }

    /// No longhand in the table is itself a shorthand.
    #[test]
    fn every_listed_longhand_is_a_longhand() {
        for (k, l) in SHORTHANDS {
            for name in *l {
                assert!(longhands(name).is_none(), "{k} lists the shorthand {name}");
            }
        }
    }

    #[test]
    fn logical_counterparts_go_both_ways() {
        assert_eq!(logical_counterparts("inline-size"), ["width", "height"]);
        let width = logical_counterparts("width");
        assert!(width.contains(&"inline-size") && width.contains(&"block-size"));
        assert_eq!(logical_counterparts("margin-left").len(), 4);
        assert!(logical_counterparts("color").is_empty());
    }
}
