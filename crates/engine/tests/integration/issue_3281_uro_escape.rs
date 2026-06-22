//! Regression for #3281 — Uro, Titan of Nature's Wrath escape from graveyard.
//!
//! https://github.com/phase-rs/phase/issues/3281

use super::support::shared_card_db;
use engine::game::casting::spell_objects_available_to_cast;
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, CastingVariant, PayCostKind, WaitingFor};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::{ExileCostSourceZone, Zone};

fn mana(color: ManaType, n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| {
            ManaUnit::new(
                color,
                engine::types::identifiers::ObjectId(0),
                false,
                vec![],
            )
        })
        .collect()
}

#[test]
fn uro_escape_castable_from_graveyard_with_five_other_cards() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let uro_id = scenario.add_real_card(P0, "Uro, Titan of Nature's Wrath", Zone::Graveyard, db);

    for idx in 0..5 {
        scenario.add_creature_to_graveyard(P0, &format!("Filler {idx}"), 1, 1);
    }

    let castable = spell_objects_available_to_cast(scenario.build().state(), P0);
    assert!(
        castable.contains(&uro_id),
        "Uro should be castable via escape with 5 other graveyard cards; castable={castable:?}"
    );
}

#[test]
fn uro_escape_not_castable_with_only_four_other_graveyard_cards() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let uro_id = scenario.add_real_card(P0, "Uro, Titan of Nature's Wrath", Zone::Graveyard, db);

    for idx in 0..4 {
        scenario.add_creature_to_graveyard(P0, &format!("Filler {idx}"), 1, 1);
    }

    let castable = spell_objects_available_to_cast(scenario.build().state(), P0);
    assert!(
        !castable.contains(&uro_id),
        "Uro must not be castable via escape with only four other graveyard cards"
    );
}

#[test]
fn uro_escape_full_cast_pauses_for_graveyard_exile_payment() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        mana(ManaType::Green, 2)
            .into_iter()
            .chain(mana(ManaType::Blue, 2))
            .collect(),
    );

    let uro_id = scenario.add_real_card(P0, "Uro, Titan of Nature's Wrath", Zone::Graveyard, db);
    let filler: Vec<_> = (0..5)
        .map(|idx| {
            scenario
                .add_creature_to_graveyard(P0, &format!("Filler {idx}"), 1, 1)
                .id()
        })
        .collect();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&uro_id].card_id;
    let result = runner
        .act(GameAction::CastSpell {
            object_id: uro_id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Uro escape cast should enter the pipeline");

    let WaitingFor::PayCost {
        kind:
            PayCostKind::ExileFromZone {
                zone: ExileCostSourceZone::Graveyard,
            },
        count,
        choices,
        ..
    } = result.waiting_for
    else {
        panic!(
            "Uro escape must pause to exile five other graveyard cards, got {:?}",
            result.waiting_for
        );
    };
    assert_eq!(count, 5);
    assert!(!choices.contains(&uro_id));
    assert!(filler.iter().all(|id| choices.contains(id)));

    let stack_variant = runner
        .state()
        .stack
        .get(0)
        .and_then(|entry| match &entry.kind {
            engine::types::game_state::StackEntryKind::Spell {
                casting_variant, ..
            } => Some(*casting_variant),
            _ => None,
        });
    assert_eq!(stack_variant, Some(CastingVariant::Escape));
}
