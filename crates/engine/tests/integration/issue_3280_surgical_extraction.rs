//! Regression for issue #3280: Surgical Extraction must exile all copies of the
//! chosen graveyard card's name from its owner's graveyard, hand, and library,
//! then shuffle that player's library.
//!
//! https://github.com/phase-rs/phase/issues/3280

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SURGICAL_EXTRACTION_ORACLE: &str = "Choose target card in a graveyard. Search its owner's graveyard, hand, and library for any number of cards with the same name as that card and exile them. Then that player shuffles.";

fn floating_mana(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

#[test]
fn surgical_extraction_exiles_same_name_cards_and_shuffles_owner_library() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let target_bolt = scenario
        .add_spell_to_graveyard(P1, "Lightning Bolt", true)
        .id();
    let other_bolt_gy = scenario
        .add_spell_to_graveyard(P1, "Lightning Bolt", true)
        .id();
    let bolt_hand = scenario.add_spell_to_hand(P1, "Lightning Bolt", true).id();
    let bolt_lib = scenario.add_spell_to_hand(P1, "Lightning Bolt", true).id();
    {
        let state = scenario.state_mut();
        state.players[1].hand.retain(|&id| id != bolt_lib);
        state.players[1].library.push(bolt_lib);
        state.objects.get_mut(&bolt_lib).unwrap().zone = Zone::Library;
    }
    let counterspell_gy = scenario
        .add_spell_to_graveyard(P1, "Counterspell", true)
        .id();

    let lib_before = scenario.state().players[1].library.clone();

    let surgical = scenario
        .add_spell_to_hand_from_oracle(P0, "Surgical Extraction", true, SURGICAL_EXTRACTION_ORACLE)
        .id();
    scenario.with_mana_pool(P0, floating_mana(1, ManaType::Black));

    let mut runner = scenario.build();
    let outcome = runner
        .cast(surgical)
        .target_objects(&[target_bolt])
        .resolve();

    for &id in &[target_bolt, other_bolt_gy, bolt_hand, bolt_lib] {
        outcome.assert_zone(&[id], Zone::Exile);
    }
    outcome.assert_zone(&[counterspell_gy], Zone::Graveyard);
    assert!(
        runner.state().players[1].graveyard.is_empty(),
        "opponent graveyard must be empty after exiling both Lightning Bolts"
    );
    assert_eq!(
        runner.state().players[1].library.len(),
        lib_before.len(),
        "library size must be preserved after the mandatory shuffle"
    );
}
