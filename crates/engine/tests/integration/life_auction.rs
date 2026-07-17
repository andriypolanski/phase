//! Integration tests for open-bid life auctions (issue #4131).

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::engine::apply;
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ControllerRef, Effect, LifeAuctionParticipants,
    LifeAuctionStartingBid, QuantityExpr, QuantityRef, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

fn illicit_auction_ability(
    controller: PlayerId,
    source: ObjectId,
    creature: ObjectId,
) -> engine::types::ability::ResolvedAbility {
    let def = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::LifeAuction {
            participants: LifeAuctionParticipants::AllPlayers,
            starting_bid: LifeAuctionStartingBid::Fixed(0),
            starting_with: ControllerRef::You,
        },
    )
    .sub_ability(
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::LoseLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::WinningBidAmount,
                },
                target: Some(TargetFilter::WinningBidder),
            },
        )
        .sub_ability(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GiveControl {
                target: TargetFilter::ParentTarget,
                recipient: TargetFilter::WinningBidder,
            },
        )),
    );
    let mut ability = build_resolved_from_def(&def, source, controller);
    ability.targets = vec![TargetRef::Object(creature)];
    ability
}

#[test]
fn two_player_auction_passes_at_zero_and_winner_gains_control() {
    let mut state = GameState::new_two_player(0);
    let controller = PlayerId(0);
    let opponent = PlayerId(1);
    let source = ObjectId(900);
    let creature = create_object(
        &mut state,
        CardId(42),
        opponent,
        "Grizzly Bears".into(),
        Zone::Battlefield,
    );

    let ability = illicit_auction_ability(controller, source, creature);
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    let WaitingFor::LifeAuctionBid {
        player, high_bid, ..
    } = state.waiting_for
    else {
        panic!(
            "expected LifeAuctionBid after auction starts, got {:?}",
            state.waiting_for
        );
    };
    assert_eq!(player, opponent);
    assert_eq!(high_bid, 0);

    apply(&mut state, opponent, GameAction::PassLifeAuction).expect("opponent passes");

    assert_eq!(
        state.objects.get(&creature).unwrap().controller,
        controller,
        "high bidder at 0 should gain control"
    );
    assert_eq!(state.players[0].life, 20, "zero bid costs no life");
}

#[test]
fn over_life_bid_is_legal_and_paid_after_auction() {
    let mut state = GameState::new_two_player(0);
    let controller = PlayerId(0);
    let opponent = PlayerId(1);
    state.players[1].life = 5;

    let ability = illicit_auction_ability(controller, ObjectId(900), ObjectId(1));
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    apply(
        &mut state,
        opponent,
        GameAction::SubmitLifeAuctionBid { amount: 10 },
    )
    .expect("over-life bid must be legal");
    apply(&mut state, controller, GameAction::PassLifeAuction).expect("controller passes");

    assert_eq!(
        state.players[1].life, -5,
        "winner pays the announced bid after the auction"
    );
}

fn scry_payoff_auction_ability(
    controller: PlayerId,
    source: ObjectId,
) -> engine::types::ability::ResolvedAbility {
    let def = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::LifeAuction {
            participants: LifeAuctionParticipants::AllPlayers,
            starting_bid: LifeAuctionStartingBid::Fixed(0),
            starting_with: ControllerRef::You,
        },
    )
    .sub_ability(
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::LoseLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::WinningBidAmount,
                },
                target: Some(TargetFilter::WinningBidder),
            },
        )
        .sub_ability(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Scry {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::WinningBidder,
            },
        )),
    );
    build_resolved_from_def(&def, source, controller)
}

#[test]
fn pains_reward_opening_bid_above_i32_max_is_preserved() {
    let mut state = GameState::new_two_player(0);
    let controller = PlayerId(0);
    let source = ObjectId(900);
    let opening_bid = (i32::MAX as u32).saturating_add(1);

    let def = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::LifeAuction {
            participants: LifeAuctionParticipants::AllPlayers,
            starting_bid: LifeAuctionStartingBid::ControllerChooses,
            starting_with: ControllerRef::You,
        },
    )
    .sub_ability(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::LoseLife {
            amount: QuantityExpr::Ref {
                qty: QuantityRef::WinningBidAmount,
            },
            target: Some(TargetFilter::WinningBidder),
        },
    ));
    let ability = build_resolved_from_def(&def, source, controller);
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    assert!(
        matches!(state.waiting_for, WaitingFor::PayAmountChoice { .. }),
        "Pain's Reward must park on PayAmountChoice, got {:?}",
        state.waiting_for
    );
    assert!(
        state.pending_life_auction_participants.is_some(),
        "participant queue must be stashed for opening bid"
    );

    apply(
        &mut state,
        controller,
        GameAction::SubmitPayAmount {
            amount: opening_bid,
        },
    )
    .expect("opening bid above i32::MAX must be accepted");

    let WaitingFor::LifeAuctionBid { high_bid, .. } = state.waiting_for else {
        panic!(
            "opening bid must begin auction at LifeAuctionBid, got {:?}",
            state.waiting_for
        );
    };
    assert_eq!(
        high_bid, opening_bid,
        "opening bid must not narrow through last_effect_count i32 wrap"
    );
    assert_eq!(
        state.last_effect_count, None,
        "auction opening bid must not stamp last_effect_count"
    );
}

#[test]
fn auction_payoff_preserves_nested_scry_choice() {
    let mut state = GameState::new_two_player(0);
    let controller = PlayerId(0);
    let opponent = PlayerId(1);
    let source = ObjectId(900);
    let scry_card = create_object(
        &mut state,
        CardId(11),
        controller,
        "Island".into(),
        Zone::Library,
    );

    let ability = scry_payoff_auction_ability(controller, source);
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

    apply(&mut state, opponent, GameAction::PassLifeAuction).expect("opponent passes");

    let WaitingFor::ScryChoice { player, ref cards } = state.waiting_for else {
        panic!(
            "auction payoff must preserve nested ScryChoice, got {:?}",
            state.waiting_for
        );
    };
    assert_eq!(player, controller, "high bidder scries");
    assert_eq!(cards.as_slice(), &[scry_card]);

    apply(
        &mut state,
        controller,
        GameAction::SelectCards {
            cards: vec![scry_card],
        },
    )
    .expect("complete scry choice");

    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "after scry completes, resolution should return to priority, got {:?}",
        state.waiting_for
    );
}
