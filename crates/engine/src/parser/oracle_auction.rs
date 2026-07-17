//! Card-defined open-bid life auction parser (Illicit Auction, Pain's Reward,
//! Mages' Contest).
//!
//! Consumes the multi-sentence auction block as a single `Effect::LifeAuction`
//! with chained payoff sub-abilities.

use nom::branch::alt;
use nom::bytes::complete::tag_no_case;
use nom::combinator::{map, opt, value};
use nom::sequence::preceded;
use nom::Parser;

use crate::parser::oracle_nom::error::OracleError;
use crate::parser::oracle_nom::primitives::parse_number;
use crate::types::ability::{
    AbilityDefinition, AbilityKind, ControllerRef, Effect, LifeAuctionParticipants,
    LifeAuctionStartingBid, QuantityExpr, QuantityRef, TargetFilter,
};

use super::oracle_effect::parse_effect_chain_with_context;
use super::oracle_ir::context::ParseContext;

/// Detect and parse a full open-bid life auction block.
pub(crate) fn parse_life_auction_block(text: &str, kind: AbilityKind) -> Option<AbilityDefinition> {
    let (rest, participants) = parse_participants_opener(text)?;
    let (rest, starting_bid) = parse_starting_bid_line(rest)?;
    let (rest, ()) = parse_auction_procedure(rest)?;
    let payoff = parse_auction_payoff(rest.trim_start(), kind)?;
    Some(
        AbilityDefinition::new(
            kind,
            Effect::LifeAuction {
                participants,
                starting_bid,
                starting_with: ControllerRef::You,
            },
        )
        .sub_ability(payoff),
    )
}

fn parse_participants_opener(i: &str) -> Option<(&str, LifeAuctionParticipants)> {
    alt((
        map(
            (
                tag_no_case::<_, _, OracleError<'_>>(
                    "each player may bid life for control of target creature",
                ),
                opt(preceded(tag_no_case("."), tag_no_case(" "))),
            ),
            |_| LifeAuctionParticipants::AllPlayers,
        ),
        map(
            (
                tag_no_case::<_, _, OracleError<'_>>("each player may bid life"),
                opt(preceded(tag_no_case("."), tag_no_case(" "))),
            ),
            |_| LifeAuctionParticipants::AllPlayers,
        ),
        map(
            (
                tag_no_case::<_, _, OracleError<'_>>("you and target spell's controller bid life"),
                opt(preceded(tag_no_case("."), tag_no_case(" "))),
            ),
            |_| LifeAuctionParticipants::ControllerAndTargetController,
        ),
    ))
    .parse(i)
    .ok()
}

fn parse_starting_bid_line(i: &str) -> Option<(&str, LifeAuctionStartingBid)> {
    preceded(
        tag_no_case::<_, _, OracleError<'_>>("you start the bidding with a bid of "),
        alt((
            value(
                LifeAuctionStartingBid::ControllerChooses,
                tag_no_case::<_, _, OracleError<'_>>("any number"),
            ),
            map(parse_number, LifeAuctionStartingBid::Fixed),
        )),
    )
    .parse(i)
    .ok()
    .map(|(rest, bid)| {
        let rest = rest
            .strip_prefix('.')
            .map(|r| r.trim_start())
            .unwrap_or(rest);
        (rest, bid)
    })
}

fn parse_auction_procedure(i: &str) -> Option<(&str, ())> {
    (
        tag_no_case::<_, _, OracleError<'_>>("in turn order, each player may top the high bid"),
        opt(preceded(tag_no_case("."), tag_no_case(" "))),
        tag_no_case::<_, _, OracleError<'_>>("the bidding ends if the high bid stands"),
        opt(preceded(tag_no_case("."), tag_no_case(" "))),
    )
        .map(|_| ())
        .parse(i)
        .ok()
}

fn parse_auction_payoff(text: &str, kind: AbilityKind) -> Option<AbilityDefinition> {
    if text.is_empty() {
        return None;
    }
    let parsed = parse_effect_chain_with_context(text, kind, &mut ParseContext::default());
    if matches!(*parsed.effect, Effect::Unimplemented { .. }) {
        return rewrite_payoff_chain(text, kind);
    }
    Some(normalize_payoff(parsed))
}

fn rewrite_payoff_chain(text: &str, kind: AbilityKind) -> Option<AbilityDefinition> {
    let lower = text.to_ascii_lowercase();
    if lower.starts_with("the high bidder loses life equal to the high bid and draws") {
        let draw = parse_effect_chain_with_context(text, kind, &mut ParseContext::default());
        return Some(build_lose_life_then(draw));
    }
    if lower.starts_with("the high bidder loses life equal to the high bid and gains control") {
        let tail = AbilityDefinition::new(
            kind,
            Effect::GiveControl {
                target: TargetFilter::ParentTarget,
                recipient: TargetFilter::WinningBidder,
            },
        );
        return Some(build_lose_life_then(tail));
    }
    if lower.starts_with("the high bidder loses life equal to the high bid.") {
        let mut lose = AbilityDefinition::new(
            kind,
            Effect::LoseLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::WinningBidAmount,
                },
                target: Some(TargetFilter::WinningBidder),
            },
        );
        let rest = text
            .strip_prefix("The high bidder loses life equal to the high bid.")
            .or_else(|| text.strip_prefix("the high bidder loses life equal to the high bid."))
            .map(str::trim_start);
        if let Some(rest) = rest.filter(|s| !s.is_empty()) {
            let cond = parse_effect_chain_with_context(rest, kind, &mut ParseContext::default());
            lose = lose.sub_ability(cond);
        }
        return Some(lose);
    }
    None
}

fn build_lose_life_then(tail: AbilityDefinition) -> AbilityDefinition {
    AbilityDefinition::new(
        tail.kind,
        Effect::LoseLife {
            amount: QuantityExpr::Ref {
                qty: QuantityRef::WinningBidAmount,
            },
            target: Some(TargetFilter::WinningBidder),
        },
    )
    .sub_ability(tail)
}

fn normalize_payoff(mut def: AbilityDefinition) -> AbilityDefinition {
    rewrite_lose_life_in_chain(&mut def);
    def
}

fn rewrite_lose_life_in_chain(def: &mut AbilityDefinition) {
    if let Effect::LoseLife { amount, target } = &mut *def.effect {
        *amount = QuantityExpr::Ref {
            qty: QuantityRef::WinningBidAmount,
        };
        if target.is_none() {
            *target = Some(TargetFilter::WinningBidder);
        }
    }
    if let Some(sub) = def.sub_ability.as_mut() {
        rewrite_lose_life_in_chain(sub);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ILLICIT: &str = "Each player may bid life for control of target creature. You start the bidding with a bid of 0. In turn order, each player may top the high bid. The bidding ends if the high bid stands. The high bidder loses life equal to the high bid and gains control of the creature.";
    const PAINS: &str = "Each player may bid life. You start the bidding with a bid of any number. In turn order, each player may top the high bid. The bidding ends if the high bid stands. The high bidder loses life equal to the high bid and draws four cards.";
    const MAGES: &str = "You and target spell's controller bid life. You start the bidding with a bid of 1. In turn order, each player may top the high bid. The bidding ends if the high bid stands. The high bidder loses life equal to the high bid. If you win the bidding, counter that spell.";

    #[test]
    fn parses_illicit_auction_block() {
        let def = parse_life_auction_block(ILLICIT, AbilityKind::Spell).expect("illicit");
        assert!(matches!(
            *def.effect,
            Effect::LifeAuction {
                participants: LifeAuctionParticipants::AllPlayers,
                starting_bid: LifeAuctionStartingBid::Fixed(0),
                ..
            }
        ));
        assert!(def.sub_ability.is_some());
    }

    #[test]
    fn parses_pains_reward_block() {
        let def = parse_life_auction_block(PAINS, AbilityKind::Spell).expect("pains");
        assert!(matches!(
            *def.effect,
            Effect::LifeAuction {
                starting_bid: LifeAuctionStartingBid::ControllerChooses,
                ..
            }
        ));
    }

    #[test]
    fn parses_mages_contest_block() {
        let def = parse_life_auction_block(MAGES, AbilityKind::Spell).expect("mages");
        assert!(matches!(
            *def.effect,
            Effect::LifeAuction {
                participants: LifeAuctionParticipants::ControllerAndTargetController,
                starting_bid: LifeAuctionStartingBid::Fixed(1),
                ..
            }
        ));
    }
}
