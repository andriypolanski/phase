//! Regression for issue #5952 — Fumiko the Lowblood "goads herself."
//!
//! Fumiko's static "Creatures your opponents control attack each combat if able."
//! is a cross-permanent `MustAttack` whose `affected` filter is scoped to the
//! controller's OPPONENTS (CR 109.5). Fumiko is controlled by you, so she is
//! outside her own requirement and must not be forced to attack.
//!
//! The bug: the must-attack enforcement had a `has_local_must_attack` fast-path
//! that reported a requirement for any creature merely CARRYING a `MustAttack`
//! static, ignoring that static's `affected` filter — so Fumiko (the carrier)
//! forced herself. The `affected` filter is the single authority for WHO must
//! attack, evaluated by `check_static_ability` against each carrier.
//!
//! Drives the REAL parse → synthesis → layer → combat-enforcement pipeline and
//! queries `creature_must_attack` — a runtime test, not an AST-shape test.

use engine::game::combat::creature_must_attack;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::PlayerId;

/// Fumiko the Lowblood (Betrayers of Kamigawa).
const FUMIKO: &str = "Fumiko the Lowblood has bushido X, where X is the number of attacking creatures. (Whenever this creature blocks or becomes blocked, it gets +X/+X until end of turn.)\nCreatures your opponents control attack each combat if able.";

fn must_attack(runner: &mut engine::game::scenario::GameRunner, id: ObjectId) -> bool {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    creature_must_attack(runner.state(), id)
}

fn build_with_fumiko(
    active: PlayerId,
) -> (
    engine::game::scenario::GameRunner,
    ObjectId,
    ObjectId,
    ObjectId,
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);

    // Fumiko under P0's control, carrying the opponent-scoped MustAttack static.
    let fumiko = scenario
        .add_creature_from_oracle(P0, "Fumiko the Lowblood", 3, 2, FUMIKO)
        .id();
    // P0's other creature — also outside "your opponents control".
    let ally = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    // Opponent's creature — inside the requirement.
    let foe = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = active;
    (runner, fumiko, ally, foe)
}

/// CR 508.1d + CR 109.5: Fumiko controls the static, so her OWN creatures — Fumiko
/// herself included — are outside "creatures your opponents control" and must NOT
/// be forced to attack during her own combat.
#[test]
fn fumiko_does_not_force_her_own_creatures_to_attack() {
    let (mut runner, fumiko, ally, _foe) = build_with_fumiko(P0);

    assert!(
        !must_attack(&mut runner, fumiko),
        "Fumiko is controlled by you, not an opponent — she must not force herself to attack",
    );
    assert!(
        !must_attack(&mut runner, ally),
        "Fumiko's controller's other creatures are outside the opponent-scoped requirement",
    );
}

/// CR 508.1d + CR 109.5: The requirement DOES force creatures Fumiko's opponents
/// control (positive control — proves the static still functions after the fix).
#[test]
fn fumiko_forces_opponents_creatures_to_attack() {
    let (mut runner, _fumiko, _ally, foe) = build_with_fumiko(P1);

    assert!(
        must_attack(&mut runner, foe),
        "a creature an opponent of Fumiko's controller controls must attack each combat",
    );
}
