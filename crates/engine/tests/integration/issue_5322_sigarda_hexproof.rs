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

const SIGARDA_ORACLE: &str = "Flying, lifelink\nYou and Humans you control have hexproof.";

fn has_creature_hexproof(runner: &mut engine::game::scenario::GameRunner, id: ObjectId) -> bool {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    has_keyword(&runner.state().objects[&id], &Keyword::Hexproof)
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
    let mut runner = scenario.build();

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
