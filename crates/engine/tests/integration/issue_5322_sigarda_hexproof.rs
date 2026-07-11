//! Regression for issue #5322: Sigarda, Heron's Grace must grant hexproof only to
//! the controller (player) and Humans you control — not to every creature you
//! control, and not to Sigarda herself (she is an Angel, not a Human).
//!
//! https://github.com/phase-rs/phase/issues/5322

use engine::game::keywords::has_keyword;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::targeting::find_legal_targets;
use engine::parser::oracle_static::parse_static_line_multi;
use engine::types::ability::{ControllerRef, TargetFilter, TypeFilter, TypedFilter};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;

const SIGARDA_ORACLE: &str = "Flying\nYou and Humans you control have hexproof.\n{2}, Exile a card from your graveyard: Create a 1/1 white Human Soldier token.";

fn has_creature_hexproof(runner: &mut engine::game::scenario::GameRunner, id: ObjectId) -> bool {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    has_keyword(&runner.state().objects[&id], &Keyword::Hexproof)
}

#[test]
fn sigarda_build_oracle_face_splits_hexproof_halves() {
    use engine::database::mtgjson::{AtomicCard, AtomicIdentifiers};
    use engine::database::synthesis::build_oracle_face;
    use engine::types::statics::StaticMode;

    let card = AtomicCard {
        name: "Sigarda, Heron's Grace".to_string(),
        mana_cost: Some("{3}{G}{W}".to_string()),
        colors: vec!["G".to_string(), "W".to_string()],
        color_identity: vec!["G".to_string(), "W".to_string()],
        power: Some("4".to_string()),
        toughness: Some("5".to_string()),
        loyalty: None,
        defense: None,
        text: Some(SIGARDA_ORACLE.to_string()),
        layout: "normal".to_string(),
        type_line: Some("Legendary Creature — Angel".to_string()),
        types: vec!["Creature".to_string()],
        subtypes: vec!["Angel".to_string()],
        supertypes: vec!["Legendary".to_string()],
        keywords: Some(vec!["Flying".to_string()]),
        side: None,
        face_name: None,
        mana_value: 5.0,
        legalities: Default::default(),
        leadership_skills: None,
        printings: Vec::new(),
        rulings: Vec::new(),
        is_game_changer: false,
        identifiers: AtomicIdentifiers {
            scryfall_oracle_id: Some("sigarda-herons-grace".to_string()),
            scryfall_id: Some("sigarda-herons-grace-face".to_string()),
        },
        foreign_data: Vec::new(),
        related_cards: engine::database::mtgjson::SetRelatedCards::default(),
    };
    let face = build_oracle_face(&card, None);
    let hexproof_statics: Vec<_> = face
        .static_abilities
        .iter()
        .filter(|s| {
            s.description
                .as_deref()
                .is_some_and(|d| d.contains("hexproof"))
                || matches!(s.mode, StaticMode::Hexproof)
                || s.modifications.iter().any(|m| {
                    matches!(
                        m,
                        engine::types::ability::ContinuousModification::AddKeyword {
                            keyword: Keyword::Hexproof
                        }
                    )
                })
        })
        .collect();
    assert_eq!(
        hexproof_statics.len(),
        2,
        "build_oracle_face must split object Continuous + player Hexproof, got {hexproof_statics:?}"
    );
}

#[test]
fn sigarda_full_oracle_parse_matches_line_split() {
    use engine::parser::oracle::parse_oracle_text;

    let oracle = SIGARDA_ORACLE;
    let parsed = parse_oracle_text(
        oracle,
        "Sigarda, Heron's Grace",
        &["flying".to_string()],
        &["Creature".to_string()],
        &["Angel".to_string()],
    );
    let hexproof_statics: Vec<_> = parsed
        .statics
        .iter()
        .filter(|s| {
            s.description
                .as_deref()
                .is_some_and(|d| d.contains("hexproof"))
        })
        .collect();
    assert_eq!(
        hexproof_statics.len(),
        2,
        "full-card parse must split object Continuous + player Hexproof, got {hexproof_statics:?}"
    );
}

#[test]
fn sigarda_hexproof_static_splits_human_object_and_player_halves() {
    let defs = parse_static_line_multi("You and Humans you control have hexproof.");
    assert_eq!(
        defs.len(),
        2,
        "expected object Continuous + player Hexproof"
    );

    let object_def = &defs[0];
    assert_eq!(object_def.mode, StaticMode::Continuous);
    let TargetFilter::Typed(tf) = object_def.affected.as_ref().expect("object affected") else {
        panic!("object-half must be Typed, got {:?}", object_def.affected);
    };
    assert_eq!(tf.controller, Some(ControllerRef::You));
    assert!(
        tf.type_filters
            .contains(&TypeFilter::Subtype("Human".to_string())),
        "object-half must filter Humans, got {:?}",
        tf.type_filters
    );

    let player_def = &defs[1];
    assert_eq!(player_def.mode, StaticMode::Hexproof);
    assert_eq!(
        player_def.affected,
        Some(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::You)
        ))
    );
}

#[test]
fn sigarda_grants_hexproof_to_humans_not_all_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let sigarda = scenario
        .add_creature_from_oracle(P0, "Sigarda, Heron's Grace", 4, 5, SIGARDA_ORACLE)
        .with_subtypes(vec!["Angel"])
        .id();
    let human = scenario
        .add_creature(P0, "Human Soldier", 2, 2)
        .with_subtypes(vec!["Human"])
        .id();
    let bear = scenario.add_creature(P0, "Grizzly Bear", 2, 2).id();

    let mut runner = scenario.build();

    assert!(
        has_creature_hexproof(&mut runner, human),
        "Humans you control must have hexproof from Sigarda"
    );
    assert!(
        !has_creature_hexproof(&mut runner, bear),
        "non-Human creatures must NOT receive Sigarda's creature hexproof"
    );
    assert!(
        !has_creature_hexproof(&mut runner, sigarda),
        "Sigarda (Angel, not Human) must NOT receive creature hexproof from the Human filter"
    );
}

#[test]
fn sigarda_player_hexproof_blocks_opponent_player_targeting() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sigarda = scenario
        .add_creature_from_oracle(P0, "Sigarda, Heron's Grace", 4, 5, SIGARDA_ORACLE)
        .with_subtypes(vec!["Angel"])
        .id();
    let runner = scenario.build();

    let opponent_source = runner
        .state()
        .objects
        .values()
        .find(|o| o.controller == P1 && o.zone == engine::types::zones::Zone::Battlefield)
        .map(|o| o.id);

    // P1 may not have a battlefield source in the default scenario; use Sigarda as
    // a stand-in controller-owned object id only for the source parameter — legality
    // is evaluated from controller perspective.
    let p1_source = opponent_source.unwrap_or(sigarda);

    let legal = find_legal_targets(runner.state(), &TargetFilter::Any, P1, p1_source);
    assert!(
        !legal.contains(&engine::types::ability::TargetRef::Player(P0)),
        "opponent must not target hexproof player P0, got {legal:?}"
    );
}
