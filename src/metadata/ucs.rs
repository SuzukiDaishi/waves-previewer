//! Offline UCS 8.2.1 category data used by the inspector.
//!
//! The official public-domain workbook is converted to a compact TSV and
//! embedded in the binary. Normalization and validation therefore never
//! depend on network state. Unknown CatIDs are preserved verbatim: read-only
//! inspection must not invent a replacement.

use std::collections::HashMap;
use std::sync::OnceLock;

pub const VERSION: &str = "8.2.1";
pub const SOURCE: &str = "https://universalcategorysystem.com/";
pub const SOURCE_WORKBOOK: &str = "UCS v8.2.1 Full List.xlsx";
pub const SOURCE_WORKBOOK_SHA256: &str =
    "e67a664803fe83e0cdf20947fb960a255f9dc087cc0df00187231891c47d76d6";
/// SHA-256 identity of the generated embedded TSV.
pub const DATA_SHA256: &str = "a75dd623f118954f5969369e4ccfd2c4f1f2b77dc83771fcd313f6da606e7773";
/// Kept as the cache-key name used by the first Metadata Inspector schema.
pub const ALIAS_TABLE_SHA256: &str = DATA_SHA256;

const UCS_DATA: &str = include_str!("data/ucs_8_2_1.tsv");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UcsCategory {
    pub category: &'static str,
    pub subcategory: &'static str,
    pub cat_id: &'static str,
    pub cat_short: &'static str,
    pub explanation: &'static str,
    pub synonyms: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UcsResolution {
    pub canonical: String,
    pub was_alias: bool,
    pub known: bool,
    pub category: Option<UcsCategory>,
}

fn category_map() -> &'static HashMap<String, UcsCategory> {
    static MAP: OnceLock<HashMap<String, UcsCategory>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut map = HashMap::with_capacity(753);
        for line in UCS_DATA.lines() {
            if line.is_empty() || line.starts_with('#') || line.starts_with("category\t") {
                continue;
            }
            let mut columns = line.splitn(6, '\t');
            let Some(category) = columns.next() else {
                continue;
            };
            let Some(subcategory) = columns.next() else {
                continue;
            };
            let Some(cat_id) = columns.next() else {
                continue;
            };
            let Some(cat_short) = columns.next() else {
                continue;
            };
            let Some(explanation) = columns.next() else {
                continue;
            };
            let synonyms = columns.next().unwrap_or_default();
            let item = UcsCategory {
                category,
                subcategory,
                cat_id,
                cat_short,
                explanation,
                synonyms,
            };
            map.insert(cat_id.to_ascii_lowercase(), item);
        }
        map
    })
}

fn alias_candidate(value: &str) -> &str {
    match value.to_ascii_uppercase().as_str() {
        // UCS 8.2.1 corrected the prior WOOD-HANDLE CatID typo.
        "WOOD-HANDLE" | "WOOD_HANDLE" | "WOODHNDL" => "WOODHndl",
        _ => value,
    }
}

/// Look up a CatID using official spelling or a supported legacy alias.
pub fn lookup_cat_id(value: &str) -> Option<UcsCategory> {
    let trimmed = value.trim();
    let candidate = alias_candidate(trimmed);
    category_map().get(&candidate.to_ascii_lowercase()).copied()
}

/// Resolve a CatID to the exact UCS 8.2.1 spelling. Unknown IDs pass through
/// unchanged and are marked as unknown so callers can emit a diagnostic.
pub fn resolve_cat_id(value: &str) -> UcsResolution {
    let trimmed = value.trim();
    let category = lookup_cat_id(trimmed);
    let canonical = category
        .map(|item| item.cat_id)
        .unwrap_or(trimmed)
        .to_string();
    UcsResolution {
        was_alias: canonical != trimmed,
        canonical,
        known: category.is_some(),
        category,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn full_v821_dataset_is_embedded() {
        assert_eq!(category_map().len(), 753);
        assert_eq!(
            format!("{:x}", Sha256::digest(UCS_DATA.as_bytes())),
            DATA_SHA256
        );
        let air = lookup_cat_id("AIRBlow").expect("AIRBlow category");
        assert_eq!(air.category, "AIR");
        assert_eq!(air.subcategory, "BLOW");
        assert!(air.synonyms.contains("Compressed"));
    }

    #[test]
    fn v821_wood_handle_alias_resolves_offline() {
        let resolved = resolve_cat_id("WOOD-HANDLE");
        assert_eq!(resolved.canonical, "WOODHndl");
        assert!(resolved.was_alias);
        assert!(resolved.known);
        assert_eq!(
            resolved.category.expect("WOOD-HANDLE category").subcategory,
            "HANDLE"
        );
    }

    #[test]
    fn known_cat_id_casing_is_canonicalized() {
        let resolved = resolve_cat_id("airblow");
        assert_eq!(resolved.canonical, "AIRBlow");
        assert!(resolved.was_alias);
        assert!(resolved.known);
    }

    #[test]
    fn unknown_cat_id_is_preserved() {
        let resolved = resolve_cat_id("CUSTOMCat");
        assert_eq!(resolved.canonical, "CUSTOMCat");
        assert!(!resolved.was_alias);
        assert!(!resolved.known);
        assert!(resolved.category.is_none());
    }
}
