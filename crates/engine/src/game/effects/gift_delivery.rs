use crate::game::players;
use crate::game::replacement::{self, ReplacementResult};
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::keywords::{GiftKind, GiftTokenSpec};
use crate::types::mana::ManaColor;
use crate::types::player::PlayerId;
use crate::types::proposed_event::{ProposedEvent, TokenCharacteristics, TokenSpec};
use crate::types::zones::EtbTapState;

/// CR 702.174: Deliver a gift to the opponent chosen when the gift was promised.
/// Gift delivery is a no-op when the gift wasn't promised (`additional_cost_paid == false`).
/// When promised, the opponent receives the gift before the spell's other effects resolve.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let kind = match &ability.effect {
        Effect::GiftDelivery { kind } => kind.clone(),
        _ => {
            return Err(EffectError::InvalidParam(
                "expected GiftDelivery effect".to_string(),
            ))
        }
    };

    // Gift delivery only fires when the gift was promised (additional cost paid).
    // When not promised, this is a no-op — the sub_ability chain continues to the
    // spell's normal effects.
    if !ability.context.additional_cost_paid {
        return Ok(());
    }

    // CR 702.174a: Deliver the promised gift to the opponent the caster chose
    // when promising the gift. In two-player games the sole opponent is chosen
    // automatically at cast time; legacy casts without a stored recipient fall
    // back to seat order for backward compatibility.
    let opponent = ability
        .context
        .gift_recipient
        .unwrap_or_else(|| players::next_player(state, ability.controller));

    // CR 702.174b: On a permanent, the gift ability triggers when the permanent enters.
    // CR 702.174j: For instants/sorceries, the gift effect always happens first.
    match kind {
        // CR 702.174e: "Gift a card" means the chosen player draws a card.
        GiftKind::Card => {
            deliver_card_draw(state, events, opponent)?;
        }
        // CR 702.174h–702.174i: Gift tokens route through the canonical
        // `CreateToken` replacement pipeline so token identity, predefined
        // abilities, and token-count modifiers (Doubling Season, etc.) apply.
        GiftKind::Token(spec) => {
            if !deliver_gift_token(state, events, opponent, ability, &spec)? {
                return Ok(());
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::GiftDelivery,
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

/// Deliver "gift a card" — opponent draws one card.
/// Routes through the single-authority `start_draw_sequence` path so
/// draw-replacement effects apply and CR 121.1's `allowed_draw_count` gate
/// honors `CantDraw` and `PerTurnDrawLimit` statics. The old direct
/// `select_cards_to_draw` call bypassed that gate for Gift draws.
fn deliver_card_draw(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    opponent: PlayerId,
) -> Result<(), EffectError> {
    // CR 614.1a + CR 614.6 + CR 704.3: The sequence driver retains replacement
    // pauses and drains post-replacement continuations in this resolution step.
    let _ = super::draw::start_draw_sequence(state, opponent, 1, events);

    Ok(())
}

/// CR 702.174 + CR 614.1a: Deliver a gift token through `ProposedEvent::CreateToken`
/// so replacement effects and the canonical token apply path run.
///
/// Returns `Ok(true)` when creation finished (including fully prevented batches),
/// `Ok(false)` when resolution paused for a replacement or counter choice.
fn deliver_gift_token(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    owner: PlayerId,
    ability: &ResolvedAbility,
    gift: &GiftTokenSpec,
) -> Result<bool, EffectError> {
    let (spec, enter_tapped) = token_spec_from_gift(gift, ability);
    let proposed = ProposedEvent::CreateToken {
        owner,
        spec: Box::new(spec),
        copy: None,
        enter_tapped,
        count: 1,
        applied: state
            .post_replacement_token_choice_applied
            .clone()
            .unwrap_or_default(),
    };

    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => Ok(
            super::token::apply_create_token_after_replacement(state, event, events),
        ),
        ReplacementResult::Prevented => Ok(true),
        ReplacementResult::NeedsChoice(player) => {
            state.waiting_for =
                crate::game::replacement::replacement_choice_waiting_for(player, state);
            Ok(false)
        }
    }
}

/// CR 111.1 + CR 702.174: Single authority converting a resolved gift token payload
/// into the `TokenSpec`/`EtbTapState` pair consumed by `CreateToken`.
fn token_spec_from_gift(
    gift: &GiftTokenSpec,
    ability: &ResolvedAbility,
) -> (TokenSpec, EtbTapState) {
    let authored_tapped = gift.enter_tapped.resolve(false);
    (
        TokenSpec {
            characteristics: TokenCharacteristics {
                display_name: gift.display_name.clone(),
                power: gift.power,
                toughness: gift.toughness,
                core_types: gift.core_types.clone(),
                subtypes: gift.subtypes.clone(),
                supertypes: vec![],
                colors: gift.colors.clone(),
                keywords: vec![],
            },
            script_name: gift.script_name.clone(),
            static_abilities: vec![],
            enter_with_counters: vec![],
            tapped: authored_tapped,
            enters_attacking: false,
            sacrifice_at: None,
            source_id: ability.source_id,
            controller: ability.controller,
            attach_to: None,
        },
        gift.enter_tapped,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones;
    use crate::types::ability::ResolvedAbility;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::zones::Zone;

    fn make_gift_ability(kind: GiftKind, promised: bool) -> ResolvedAbility {
        let mut ability = ResolvedAbility::new(
            Effect::GiftDelivery { kind },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.context.additional_cost_paid = promised;
        ability
    }

    #[test]
    fn gift_card_opponent_draws_when_promised() {
        let mut state = GameState::new_two_player(42);
        let card_id = zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Opponent Card".to_string(),
            Zone::Library,
        );
        let mut events = Vec::new();

        let ability = make_gift_ability(GiftKind::Card, true);
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.players[1].hand.contains(&card_id));
        assert!(events.iter().any(
            |e| matches!(e, GameEvent::CardDrawn { player_id, .. } if *player_id == PlayerId(1))
        ));
    }

    #[test]
    fn gift_card_noop_when_not_promised() {
        let mut state = GameState::new_two_player(42);
        let card_id = zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Opponent Card".to_string(),
            Zone::Library,
        );
        let mut events = Vec::new();

        let ability = make_gift_ability(GiftKind::Card, false);
        resolve(&mut state, &ability, &mut events).unwrap();

        // Opponent should NOT have drawn
        assert!(state.players[1].library.contains(&card_id));
        assert!(!events
            .iter()
            .any(|e| matches!(e, GameEvent::CardDrawn { .. })));
    }

    #[test]
    fn gift_treasure_creates_token_for_opponent() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        let ability = make_gift_ability(GiftKind::Token(GiftTokenSpec::treasure()), true);
        resolve(&mut state, &ability, &mut events).unwrap();

        let token = state
            .objects
            .values()
            .find(|o| o.card_id == CardId(0) && o.owner == PlayerId(1));
        assert!(token.is_some(), "Treasure token should exist for opponent");
        let token = token.unwrap();
        assert!(token.is_token, "gift Treasure must be a token");
        assert!(token.card_types.subtypes.contains(&"Treasure".to_string()));
        assert!(token.card_types.core_types.contains(&CoreType::Artifact));
    }

    #[test]
    fn gift_tapped_fish_creates_tapped_token() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        let ability = make_gift_ability(GiftKind::Token(GiftTokenSpec::tapped_fish()), true);
        resolve(&mut state, &ability, &mut events).unwrap();

        let token = state
            .objects
            .values()
            .find(|o| o.card_id == CardId(0) && o.owner == PlayerId(1));
        assert!(token.is_some(), "Fish token should exist for opponent");
        let token = token.unwrap();
        assert!(token.is_token, "gift Fish must be a token");
        assert_eq!(token.power, Some(1));
        assert_eq!(token.toughness, Some(1));
        assert!(token.tapped, "Fish should enter tapped");
        assert!(token.color.contains(&ManaColor::Blue));
    }

    #[test]
    fn gift_food_creates_food_token_for_opponent() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        let ability = make_gift_ability(GiftKind::Token(GiftTokenSpec::food()), true);
        resolve(&mut state, &ability, &mut events).unwrap();

        let token = state
            .objects
            .values()
            .find(|o| o.card_id == CardId(0) && o.owner == PlayerId(1));
        assert!(token.is_some(), "Food token should exist for opponent");
        let token = token.unwrap();
        assert!(token.is_token, "gift Food must be a token");
        assert!(token.card_types.subtypes.contains(&"Food".to_string()));
    }

    #[test]
    fn gift_creature_token_creates_octopus_for_opponent() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        let ability = make_gift_ability(
            GiftKind::Token(GiftTokenSpec::creature(
                "Octopus",
                8,
                8,
                vec![ManaColor::Blue],
                vec!["Octopus".to_string()],
                EtbTapState::Unspecified,
            )),
            true,
        );
        resolve(&mut state, &ability, &mut events).unwrap();

        let token = state
            .objects
            .values()
            .find(|o| o.card_id == CardId(0) && o.owner == PlayerId(1));
        assert!(token.is_some(), "Octopus token should exist for opponent");
        let token = token.unwrap();
        assert!(token.is_token, "gift Octopus must be a token");
        assert_eq!(token.power, Some(8));
        assert_eq!(token.toughness, Some(8));
        assert!(token.color.contains(&ManaColor::Blue));
        assert!(token.card_types.subtypes.contains(&"Octopus".to_string()));
    }

    #[test]
    fn gift_creature_token_doubles_under_recipient_doubling_season() {
        use crate::parser::oracle::parse_oracle_text;
        use std::sync::Arc;

        let mut state = GameState::new_two_player(42);
        let ds_id = zones::create_object(
            &mut state,
            CardId(99),
            PlayerId(1),
            "Doubling Season".to_string(),
            Zone::Battlefield,
        );
        let parsed = parse_oracle_text(
            "If one or more tokens would be created under your control, twice that \
             many tokens are created instead.",
            "Doubling Season",
            &[],
            &["Enchantment".to_string()],
            &[],
        );
        assert!(
            !parsed.replacements.is_empty(),
            "Doubling Season token doubler must parse"
        );
        {
            let obj = state.objects.get_mut(&ds_id).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            let reps = parsed.replacements.clone();
            obj.replacement_definitions = reps.clone().into();
            obj.base_replacement_definitions = Arc::new(reps);
        }
        crate::types::game_state::TriggerIndex::rebuild_from_battlefield(&mut state);

        let mut events = Vec::new();
        let ability = make_gift_ability(
            GiftKind::Token(GiftTokenSpec::creature(
                "Octopus",
                8,
                8,
                vec![ManaColor::Blue],
                vec!["Octopus".to_string()],
                EtbTapState::Unspecified,
            )),
            true,
        );
        resolve(&mut state, &ability, &mut events).unwrap();

        let octopus_tokens: Vec<_> = state
            .objects
            .values()
            .filter(|o| {
                o.is_token
                    && o.owner == PlayerId(1)
                    && o.card_types.subtypes.iter().any(|s| s == "Octopus")
            })
            .collect();
        assert_eq!(
            octopus_tokens.len(),
            2,
            "recipient's Doubling Season must double the gifted Octopus token"
        );
    }
}
