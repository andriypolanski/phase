//! Integration regression for GitHub issue #5975 — Octomancer Gift an Octopus.
//!
//! Oracle: `Gift an Octopus (… If you do, when it enters, they create an 8/8
//! blue Octopus creature token.)`
//!
//! Parser coverage lives in `oracle_keyword.rs`; this test drives the production
//! cast → ETB gift-delivery trigger path and verifies the opponent receives the
//! promised Octopus token at runtime.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::{GiftKind, Keyword};
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const OCTOMANCER_ORACLE: &str = "Gift an Octopus (You may promise an opponent a gift as you cast this spell. If you do, when it enters, they create an 8/8 blue Octopus creature token.)\nAt the beginning of each end step, create a token that's a copy of target creature token that entered the battlefield this turn.";

fn drive_octomancer_cast(
    runner: &mut GameRunner,
    spell: engine::types::ObjectId,
    promise_gift: bool,
) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Octomancer");

    for _ in 0..64 {
        match &runner.state().waiting_for {
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pay mana");
            }
            WaitingFor::OptionalCostChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalCost { pay: promise_gift })
                    .expect("decide Gift an Octopus");
            }
            WaitingFor::OrderTriggers { .. } | WaitingFor::Priority { .. }
                if !runner.state().stack.is_empty() =>
            {
                runner
                    .act(GameAction::PassPriority)
                    .expect("advance Octomancer resolution");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            other => panic!("unexpected Octomancer cast prompt: {other:?}"),
        }
    }

    panic!("Octomancer cast did not finish within 64 actions");
}

fn opponent_octopus_token(runner: &GameRunner) -> Option<ObjectId> {
    runner.state().objects.iter().find_map(|(&id, obj)| {
        (obj.owner == P1
            && obj.card_id == CardId(0)
            && obj.zone == Zone::Battlefield
            && obj.card_types.core_types.contains(&CoreType::Creature)
            && obj.card_types.subtypes.iter().any(|s| s == "Octopus"))
        .then_some(id)
    })
}

#[test]
fn octomancer_oracle_parses_gift_creature_token_and_etb_delivery_trigger() {
    let mut scenario = GameScenario::new();
    let octomancer = {
        let mut builder =
            scenario.add_creature_to_hand_from_oracle(P0, "Octomancer", 3, 3, OCTOMANCER_ORACLE);
        builder.with_mana_cost(ManaCost::zero());
        builder.id()
    };
    let runner = scenario.build();
    let obj = &runner.state().objects[&octomancer];
    assert!(
        obj.keywords
            .iter()
            .any(|k| matches!(k, Keyword::Gift(GiftKind::CreatureToken(_)))),
        "Octomancer must parse Gift an Octopus, got keywords: {:?}",
        obj.keywords
    );
    assert!(
        obj.additional_cost.is_some(),
        "synthesize_gift must wire optional additional cost"
    );
    assert!(
        obj.base_trigger_definitions.iter().any(|t| {
            matches!(
                t.execute.as_deref(),
                Some(a) if matches!(*a.effect, Effect::GiftDelivery { kind: GiftKind::CreatureToken(_) })
            )
        }),
        "permanent gift must synthesize ETB GiftDelivery trigger, got triggers: {:?}",
        obj
            .base_trigger_definitions
            .iter()
            .map(|t| (&t.mode, t.execute.as_ref().map(|a| &a.effect)))
            .collect::<Vec<_>>()
    );
}

#[test]
fn octomancer_promised_gift_delivers_octopus_token_on_etb() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let octomancer = {
        let mut builder =
            scenario.add_creature_to_hand_from_oracle(P0, "Octomancer", 3, 3, OCTOMANCER_ORACLE);
        builder.with_mana_cost(ManaCost::zero());
        builder.id()
    };

    let mut runner = scenario.build();
    drive_octomancer_cast(&mut runner, octomancer, true);

    assert_eq!(
        runner.state().objects[&octomancer].zone,
        Zone::Battlefield,
        "Octomancer must enter the battlefield after resolving"
    );

    let token_id =
        opponent_octopus_token(&runner).expect("opponent must receive Octopus gift token");
    let token = &runner.state().objects[&token_id];
    assert_eq!(token.power, Some(8));
    assert_eq!(token.toughness, Some(8));
    assert!(
        token.color.contains(&ManaColor::Blue),
        "Octopus gift token must be blue, got {:?}",
        token.color
    );
}

#[test]
fn octomancer_unpromised_gift_does_not_create_octopus_token() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let octomancer = {
        let mut builder =
            scenario.add_creature_to_hand_from_oracle(P0, "Octomancer", 3, 3, OCTOMANCER_ORACLE);
        builder.with_mana_cost(ManaCost::zero());
        builder.id()
    };

    let mut runner = scenario.build();
    drive_octomancer_cast(&mut runner, octomancer, false);

    assert!(
        opponent_octopus_token(&runner).is_none(),
        "declining Gift an Octopus must not create the Octopus token"
    );
}
