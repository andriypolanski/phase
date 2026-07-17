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
