//! Issue #4271 — Birthing Ritual must defer the looked-at pile's mana-value filter
//! until after the player sacrifices a creature, so X resolves to (sacrificed MV + 1)
//! rather than defaulting to 1.
//!
//! https://github.com/phase-rs/phase/issues/4271

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const BIRTHING_RITUAL_ORACLE: &str = "At the beginning of your end step, if you control a creature, look at the top seven cards of your library. Then you may sacrifice a creature. If you do, you may put a creature card with mana value X or less from among those cards onto the battlefield, where X is 1 plus the sacrificed creature's mana value. Put the rest on the bottom of your library in a random order.";

fn reach_end_step_trigger_on_stack(
    runner: &mut GameRunner,
    ritual: engine::types::identifiers::ObjectId,
) {
    runner.advance_to_end_step();
    for _ in 0..48 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declare empty attackers");
            }
            WaitingFor::OrderTriggers { .. } => {
                runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .ok();
            }
            WaitingFor::Priority { .. }
                if runner.state().stack.iter().any(|entry| {
                    matches!(
                        &entry.kind,
                        StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == ritual
                    )
                }) =>
            {
                return;
            }
            WaitingFor::Priority { .. } => runner.pass_both_players(),
            _ if runner.state().phase == Phase::End => return,
            _ => runner.pass_both_players(),
        }
    }
}

fn seed_library_creature(
    scenario: &mut GameScenario,
    name: &str,
) -> engine::types::identifiers::ObjectId {
    scenario.add_card_to_library_top(P0, name)
}

fn mark_library_creature(
    runner: &mut GameRunner,
    id: engine::types::identifiers::ObjectId,
    mana_value: u32,
) {
    let obj = runner
        .state_mut()
        .objects
        .get_mut(&id)
        .expect("library card");
    obj.card_types.core_types.push(CoreType::Creature);
    obj.mana_cost = ManaCost::generic(mana_value);
}

#[test]
fn birthing_ritual_dig_choice_respects_sacrificed_creature_mana_value() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ritual = scenario
        .add_creature(P0, "Birthing Ritual", 0, 0)
        .as_enchantment()
        .from_oracle_text(BIRTHING_RITUAL_ORACLE)
        .id();

    let sacrifice_target = scenario
        .add_creature(P0, "Grizzly Bears", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let _other_creature = scenario.add_creature(P0, "Elvish Mystic", 1, 1).id();

    let mv3 = seed_library_creature(&mut scenario, "Three Drop");
    let mv5 = seed_library_creature(&mut scenario, "Five Drop");
    for i in 0..5 {
        scenario.add_card_to_library_top(P0, &format!("Filler {i}"));
    }

    let mut runner = scenario.build();
    mark_library_creature(&mut runner, mv3, 3);
    mark_library_creature(&mut runner, mv5, 5);
    reach_end_step_trigger_on_stack(&mut runner, ritual);

    runner.pass_both_players();
    runner.advance_until_stack_empty();

    if matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { .. }
    ) {
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .expect("accept optional sacrifice");
    }

    let WaitingFor::EffectZoneChoice { cards, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "expected EffectZoneChoice to sacrifice a creature, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(
        cards.contains(&sacrifice_target),
        "Grizzly Bears must be eligible to sacrifice"
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![sacrifice_target],
        })
        .expect("sacrifice Grizzly Bears");

    let WaitingFor::DigChoice {
        selectable_cards, ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected DigChoice after sacrifice (MV 2 + 1 = 3 bound), got {:?}",
            runner.state().waiting_for
        );
    };

    assert!(
        selectable_cards.contains(&mv3),
        "MV 3 creature must be selectable when sacrificing MV 2 (bound = 3); got {selectable_cards:?}"
    );
    assert!(
        !selectable_cards.contains(&mv5),
        "MV 5 creature must NOT be selectable when bound is 3; got {selectable_cards:?}"
    );
}
