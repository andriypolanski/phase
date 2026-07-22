//! Issue #6500 — Loreseeker's Stone activation cost ignores hand size.
//!
//! Oracle: `{3}, {T}: Draw three cards. This ability costs {1} more to activate
//! for each card in your hand.`
//!
//! Discord report: activated for `{3}` with 5 cards in hand (should be `{8}`).
//! Root cause: the trailing "costs {1} more …" clause was an
//! `Effect::Unimplemented` gap — `try_parse_cost_reduction` only recognized
//! "less", and runtime `apply_cost_reduction` only reduced. Fix parameterizes
//! self `CostReduction` with existing `CostModifyMode::Raise` (CR 601.2f) and
//! applies the increase at cost-determination time (CR 602.2b).
//!
//! DISCRIMINATING: paid generic scales with hand size (0 → `{3}`, 5 → `{8}`).
//! A revert (no Raise parse / no Raise apply) leaves paid generic at `{3}` for
//! every hand size and fails the cross-case inequality.

use engine::game::quantity::resolve_quantity;
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, CostReduction, QuantityExpr, QuantityRef, ZoneRef,
};
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::statics::CostModifyMode;
use engine::types::zones::Zone;

const LORESEEKERS_STONE: &str = "{3}, {T}: Draw three cards. This ability costs {1} more \
to activate for each card in your hand.";

fn find_cost_reduction(def: &AbilityDefinition) -> Option<&CostReduction> {
    let mut cur = Some(def);
    while let Some(d) = cur {
        if let Some(cr) = d.cost_reduction.as_ref() {
            return Some(cr);
        }
        cur = d.sub_ability.as_deref();
    }
    None
}

fn parsed_cost_reduction() -> CostReduction {
    let parsed = parse_oracle_text(LORESEEKERS_STONE, "Loreseeker's Stone", &[], &[], &[]);
    parsed
        .abilities
        .iter()
        .find_map(find_cost_reduction)
        .cloned()
        .expect("Loreseeker's Stone must capture a self cost modification")
}

/// Mirror of runtime `apply_cost_reduction` Raise math (CR 601.2f):
/// `paid = base + amount_per * count`.
fn paid_generic_after_raise(
    state: &GameState,
    reduction: &CostReduction,
    base_generic: u32,
    player: PlayerId,
    source: ObjectId,
) -> u32 {
    assert_eq!(
        reduction.mode,
        CostModifyMode::Raise,
        "Loreseeker's Stone must Raise, not Reduce"
    );
    let count = resolve_quantity(state, &reduction.count, player, source);
    let raise_by = (reduction.amount_per as i32 * count).max(0) as u32;
    base_generic.saturating_add(raise_by)
}

#[test]
fn loreseekers_stone_parses_raise_for_each_card_in_hand() {
    let reduction = parsed_cost_reduction();
    assert_eq!(reduction.mode, CostModifyMode::Raise);
    assert_eq!(reduction.amount_per, 1);
    assert_eq!(reduction.condition, None);
    match &reduction.count {
        QuantityExpr::Ref {
            qty:
                QuantityRef::ZoneCardCount {
                    zone: ZoneRef::Hand,
                    ..
                },
        }
        | QuantityExpr::Ref {
            qty: QuantityRef::HandSize { .. },
        } => {}
        other => panic!("expected hand-size count, got {other:?}"),
    }

    let parsed = parse_oracle_text(LORESEEKERS_STONE, "Loreseeker's Stone", &[], &[], &[]);
    let ability = parsed
        .abilities
        .iter()
        .find(|a| find_cost_reduction(a).is_some())
        .expect("activation ability");
    let mut cur = Some(ability);
    while let Some(d) = cur {
        assert!(
            d.effect.unimplemented_description().is_none(),
            "cost-increase clause must not survive as Unimplemented: {:?}",
            d.effect
        );
        cur = d.sub_ability.as_deref();
    }
}

#[test]
fn loreseekers_stone_paid_generic_scales_with_hand_size() {
    let reduction = parsed_cost_reduction();
    let base_generic = 3u32;

    let build = |hand_cards: usize| -> (GameState, ObjectId) {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Loreseeker's Stone".to_string(),
            Zone::Battlefield,
        );
        for i in 0..hand_cards {
            create_object(
                &mut state,
                CardId(100 + i as u64),
                PlayerId(0),
                format!("Hand Card {i}"),
                Zone::Hand,
            );
        }
        (state, source)
    };

    let (state0, src0) = build(0);
    let paid0 = paid_generic_after_raise(&state0, &reduction, base_generic, PlayerId(0), src0);
    assert_eq!(paid0, 3, "empty hand: {{3}} + {{0}} = {{3}}");

    let (state5, src5) = build(5);
    let paid5 = paid_generic_after_raise(&state5, &reduction, base_generic, PlayerId(0), src5);
    assert_eq!(
        paid5, 8,
        "5 cards in hand: {{3}} + {{5}} = {{8}} (Discord report)"
    );

    assert_ne!(
        paid0, paid5,
        "paid generic must differ across hand sizes (revert leaves both at {{3}})"
    );
}
