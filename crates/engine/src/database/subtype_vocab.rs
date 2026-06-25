//! CR 205.3: Validated subtype vocabulary for Oracle parsing.
//!
//! Creature subtypes are harvested from MTGJSON only when corroborated by the
//! printed `type_line` subtype section and passing lexical validation. Noncreature
//! subtypes are sourced from the canonical static tables in `card_type.rs`.

use std::collections::BTreeSet;

use crate::database::mtgjson::AtomicCardsFile;
use crate::types::card_type::fixed_noncreature_subtypes;

/// CR 205.3: Lexical guard for subtype spellings. Rejects MTGJSON pollution and
/// Oracle sentence fragments that are not valid subtype names.
pub fn is_valid_subtype_spelling(candidate: &str) -> bool {
    let candidate = candidate.trim();
    if candidate.is_empty() || candidate.len() > 48 {
        return false;
    }
    let mut chars = candidate.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_uppercase() {
        return false;
    }
    for c in chars {
        if !(c.is_ascii_alphabetic() || c == ' ' || c == '-' || c == '\'') {
            return false;
        }
    }
    // Bare English function words / known MTGJSON garbage tokens.
    !matches!(
        candidate,
        "The" | "You" | "and" | "or" | "and/or" | "of" | "Baddest," | "Elemental?"
    )
}

/// Returns the subtype section of a Scryfall/MTGJSON type line (after the em dash).
pub fn subtype_section_from_type_line(type_line: &str) -> Option<&str> {
    type_line
        .split('—')
        .nth(1)
        .map(str::trim)
        .filter(|section| !section.is_empty())
}

/// A subtype from MTGJSON is parser-authoritative only when it appears in the
/// printed type line's subtype section (CR 205.3m type-line authority).
pub fn type_line_corroborates_subtype(type_line: &str, subtype: &str) -> bool {
    let Some(section) = subtype_section_from_type_line(type_line) else {
        return false;
    };
    section
        .to_ascii_lowercase()
        .contains(&subtype.to_ascii_lowercase())
}

/// Harvest creature (and other em-dash) subtypes from AtomicCards with validation.
pub fn harvest_creature_subtypes_from_atomic(atomic: &AtomicCardsFile) -> BTreeSet<String> {
    let mut subtypes = BTreeSet::new();
    for faces in atomic.data.values() {
        for face in faces {
            let Some(type_line) = face.type_line.as_deref() else {
                continue;
            };
            for subtype in &face.subtypes {
                if !is_valid_subtype_spelling(subtype) {
                    continue;
                }
                if type_line_corroborates_subtype(type_line, subtype) {
                    subtypes.insert(subtype.clone());
                }
            }
        }
    }
    subtypes
}

/// Full parser subtype vocabulary: canonical noncreature tables plus validated
/// harvested creature subtypes. Sorted longest-first for prefix-safe matching.
pub fn build_parser_subtype_vocabulary(harvested_creature: &[String]) -> Vec<String> {
    let mut set: BTreeSet<String> = BTreeSet::new();
    for subtype in fixed_noncreature_subtypes() {
        set.insert(subtype.to_string());
    }
    for subtype in harvested_creature {
        if is_valid_subtype_spelling(subtype) {
            set.insert(subtype.clone());
        }
    }
    let mut list: Vec<String> = set.into_iter().collect();
    list.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_common_oracle_words_and_mtgjson_garbage() {
        for garbage in [
            "The",
            "You",
            "and/or",
            "of",
            "Baddest,",
            "Elemental?",
            "creature",
        ] {
            assert!(
                !is_valid_subtype_spelling(garbage),
                "{garbage} must not be a subtype"
            );
        }
    }

    #[test]
    fn accepts_legitimate_subtype_spellings() {
        for good in [
            "Human",
            "Time Lord",
            "Jace",
            "Nahiri",
            "Plains",
            "Equipment",
            "Sliver",
            "Phyrexian",
        ] {
            assert!(
                is_valid_subtype_spelling(good),
                "{good} should be a valid subtype spelling"
            );
        }
    }

    #[test]
    fn type_line_corroboration_requires_em_dash_section() {
        assert!(type_line_corroborates_subtype(
            "Legendary Creature — Human Wizard",
            "Human"
        ));
        assert!(type_line_corroborates_subtype(
            "Legendary Creature — Time Lord",
            "Time Lord"
        ));
        assert!(!type_line_corroborates_subtype("Creature", "Human"));
        assert!(!type_line_corroborates_subtype(
            "Legendary Creature — Human Wizard",
            "The"
        ));
    }
}
