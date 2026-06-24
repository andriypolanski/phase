//! Regression for issue #3993: Cancelling during delve mana payment must return
//! delved graveyard cards from exile instead of leaving them stranded.
//!
//! CR 601.2i: If the player is unable or unwilling to complete a cast, the
//! process is reversed and any choices made (including delve exiles) are undone.
//!
//! https://github.com/phase-rs/phase/issues/3993

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, ConvokeMode, WaitingFor};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const DELVE_DRAW_ORACLE: &str =
    "Delve (Each card you exile from your graveyard while casting this spell pays for {1}.)\n\
Draw a card.";

fn add_mana(runner: &mut engine::game::scenario::GameRunner, ty: ManaType, count: usize) {
    let pool = &mut runner.state_mut().players[P0.0 as usize].mana_pool;
    for _ in 0..count {
        pool.add(ManaUnit::new(
            ty,
            engine::types::identifiers::ObjectId(0),
            false,
            vec![],
        ));
    }
}

#[test]
fn issue_3993_cancel_during_delve_payment_returns_graveyard_cards() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Delve Draw", false, DELVE_DRAW_ORACLE)
        .id();
    let delve_a = scenario
        .add_spell_to_graveyard(P0, "Old Bolt", true)
        .id();
    let delve_b = scenario
        .add_spell_to_graveyard(P0, "Old Shock", true)
        .id();

    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Colorless, 3);
    add_mana(&mut runner, ManaType::Red, 1);

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("begin casting delve spell");

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment {
            convoke_mode: Some(ConvokeMode::Delve),
            ..
        }
    ));

    for gy_id in [delve_a, delve_b] {
        runner
            .act(GameAction::TapForConvoke {
                object_id: gy_id,
                mana_type: ManaType::Colorless,
            })
            .expect("delve graveyard card");
        assert_eq!(
            runner.state().objects[&gy_id].zone,
            Zone::Exile,
            "delved card should be exiled before cancel"
        );
    }

    runner
        .act(GameAction::CancelCast)
        .expect("cancel during delve payment");

    for gy_id in [delve_a, delve_b] {
        assert_eq!(
            runner.state().objects[&gy_id].zone,
            Zone::Graveyard,
            "cancelled delve payment must return the card to the graveyard"
        );
    }
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Hand,
        "cancelled spell must return to hand"
    );
    assert!(
        runner.state().stack.is_empty(),
        "cancelled spell must be removed from the stack"
    );
    assert!(
        !runner
            .state()
            .cards_exiled_with_source_this_turn
            .get(&spell)
            .is_some_and(|ids| ids.contains(&delve_a) || ids.contains(&delve_b)),
        "delve exile-with-source tracking must be cleared on cancel"
    );
    assert!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .mana
            .iter()
            .all(|unit| !unit.is_convoke_payment()),
        "delve mana markers must be removed from the pool on cancel"
    );
}
