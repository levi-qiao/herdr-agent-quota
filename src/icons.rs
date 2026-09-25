//! Vendor marks for the sidebar identity row.
//!
//! Prefer Private Use Area glyphs from the bundled Herdr Agent Icons Max
//! face (same marks herdr-radar ships). Muse has no glyph in that face, so
//! it keeps a one-cell text stand-in. Terminals need the codepoint map that
//! [`crate::configure::font`] writes for Ghostty / kitty; without it the
//! PUA cells render as missing glyphs.

use crate::model::Harness;

/// Invisible suffix on `$quota_icon` so working/done/blocked colour can live on the
/// first identity token. A later twin (`$quota_icon_done`) on a Space-head
/// row hang-indents one cell to the right because the leading empty slots
/// still eat the group indent.
///
/// These must be `Grapheme_Cluster_Break=Control`, not Extend. U+200C
/// (ZWNJ) is Extend: it joins the vendor PUA glyph into one cluster, the
/// icon font has no ZWNJ, and the cell renders as a yellow `?`.
pub const WORKING_TAG: &str = "\u{2061}";
pub const DONE_TAG: &str = "\u{2060}";
pub const BLOCKED_TAG: &str = "\u{2062}";

/// One-cell mark for a harness.
pub fn for_harness(harness: Harness) -> &'static str {
    match harness {
        Harness::Claude => "\u{e1a0}",
        Harness::Codex => "\u{e1a1}",
        Harness::OpenCode => "\u{e1a2}",
        Harness::Omp => "\u{e1a3}",
        Harness::Pi => "\u{e1a9}",
        Harness::Cursor => "\u{e1ab}",
        Harness::Grok => "\u{e1b1}",
        Harness::Agy => "\u{e1b2}",
        Harness::Devin => "\u{e1b5}",
        // Not in HerdrAgentIconsMax; keep a plain mark rather than a tofu.
        Harness::Muse => "◈",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::AgentSelection;

    #[test]
    fn every_supported_harness_has_a_one_cell_mark() {
        for harness in AgentSelection::SUPPORTED {
            let mark = for_harness(harness);
            assert_eq!(mark.chars().count(), 1, "{harness:?} -> {mark:?}");
        }
    }

    #[test]
    fn supported_harnesses_use_the_radar_pua_except_muse() {
        assert_eq!(for_harness(Harness::Claude), "\u{e1a0}");
        assert_eq!(for_harness(Harness::Codex), "\u{e1a1}");
        assert_eq!(for_harness(Harness::Grok), "\u{e1b1}");
        assert_eq!(for_harness(Harness::Agy), "\u{e1b2}");
        assert_eq!(for_harness(Harness::Cursor), "\u{e1ab}");
        assert_eq!(for_harness(Harness::Muse), "◈");
    }

    #[test]
    fn status_tags_do_not_join_the_vendor_glyph() {
        assert_ne!(
            WORKING_TAG, "\u{200c}",
            "ZWNJ extends U+E1AB and the icon font draws a replacement ?"
        );
        assert_ne!(WORKING_TAG, DONE_TAG);
        assert_ne!(WORKING_TAG, BLOCKED_TAG);
        assert_ne!(DONE_TAG, BLOCKED_TAG);
        for tag in [WORKING_TAG, DONE_TAG, BLOCKED_TAG] {
            assert_ne!(tag, "\u{200c}");
            let marked = format!("{}{tag}", for_harness(Harness::Cursor));
            assert!(marked.starts_with('\u{e1ab}'));
            assert_eq!(marked.chars().count(), 2);
        }
    }
}
