//! CR 205.3: Validated subtype vocabulary for Oracle parsing.
//!
//! Creature subtypes are sourced from MTGJSON `CardTypes.json` (the canonical
//! Wizards list, including token-only types such as Army and Germ). Noncreature
//! subtypes come from the static tables in `card_type.rs`. AtomicCards face
//! harvesting remains available for validation/cross-checks only.

use std::collections::BTreeSet;

use crate::database::mtgjson::{AtomicCardsFile, CardTypesFile};
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

/// A subtype from MTGJSON AtomicCards is corroborated only when it appears in the
/// printed type line's subtype section (CR 205.3m type-line authority).
pub fn type_line_corroborates_subtype(type_line: &str, subtype: &str) -> bool {
    let Some(section) = subtype_section_from_type_line(type_line) else {
        return false;
    };
    section
        .to_ascii_lowercase()
        .contains(&subtype.to_ascii_lowercase())
}

/// CR 205.3m: Canonical creature subtypes from MTGJSON CardTypes.json. Includes
/// token-only types (Army, Germ, Servo, Tentacle, …) that never appear on card
/// faces in AtomicCards but do appear in Oracle/token text.
pub fn canonical_creature_subtypes_from_card_types(card_types: &CardTypesFile) -> BTreeSet<String> {
    card_types
        .data
        .creature
        .sub_types
        .iter()
        .filter(|subtype| is_valid_subtype_spelling(subtype))
        .cloned()
        .collect()
}

/// Harvest creature subtypes from AtomicCards with type-line corroboration.
/// Cross-check / supplemental only — parser authority is `CardTypes.json`.
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
/// creature subtypes (from committed `oracle-subtypes.json`). Sorted longest-first
/// for prefix-safe matching.
pub fn build_parser_subtype_vocabulary(creature_subtypes: &[String]) -> Vec<String> {
    let mut set: BTreeSet<String> = BTreeSet::new();
    for subtype in fixed_noncreature_subtypes() {
        set.insert(subtype.to_string());
    }
    for subtype in creature_subtypes {
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
    use crate::database::mtgjson::CardTypeEntry;

    fn fixture_card_types(creature: &[&str]) -> CardTypesFile {
        CardTypesFile {
            data: crate::database::mtgjson::CardTypesData {
                creature: CardTypeEntry {
                    sub_types: creature.iter().map(|s| (*s).to_string()).collect(),
                    super_types: Vec::new(),
                },
            },
        }
    }

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
    fn canonical_creature_subtypes_include_token_only_types() {
        let card_types = fixture_card_types(&[
            "Human",
            "Army",
            "Germ",
            "Servo",
            "Tentacle",
            "Camarid",
            "Tetravite",
        ]);
        let subtypes = canonical_creature_subtypes_from_card_types(&card_types);
        for token_subtype in ["Army", "Germ", "Servo", "Tentacle", "Camarid", "Tetravite"] {
            assert!(
                subtypes.contains(token_subtype),
                "{token_subtype} must be in canonical creature subtypes"
            );
        }
    }

    #[test]
    fn build_parser_vocabulary_includes_token_creature_subtypes() {
        let creature = canonical_creature_subtypes_from_card_types(&fixture_card_types(&[
            "Human", "Army", "Germ", "Servo", "Tentacle",
        ]));
        let vocab = build_parser_subtype_vocabulary(&creature.into_iter().collect::<Vec<_>>());
        for token_subtype in ["Army", "Germ", "Servo", "Tentacle"] {
            assert!(
                vocab.iter().any(|s| s == token_subtype),
                "{token_subtype} must be parser-authoritative"
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
