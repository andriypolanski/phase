//! Card-defined open-bid life auction parser (Illicit Auction, Pain's Reward,
//! Mages' Contest).
//!
//! Consumes the multi-sentence auction block as a single `Effect::LifeAuction`
//! with chained payoff sub-abilities.

use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::bytes::complete::tag_no_case;
use nom::character::complete::multispace0;
use nom::combinator::{map, opt, value};
use nom::sequence::{preceded, terminated};
use nom::Parser;

use crate::parser::oracle_effect::conditions::strip_auction_winner_conditional;
use crate::parser::oracle_effect::parse_effect_chain_with_context;
use crate::parser::oracle_ir::context::ParseContext;
use crate::parser::oracle_nom::bridge::nom_on_lower;
use crate::parser::oracle_nom::error::OracleError;
use crate::parser::oracle_nom::primitives::parse_number;
use crate::types::ability::{
    AbilityDefinition, AbilityKind, ControllerRef, Effect, LifeAuctionParticipants,
    LifeAuctionStartingBid, QuantityExpr, QuantityRef, TargetFilter,
};

/// Detect and parse a full open-bid life auction block.
pub(crate) fn parse_life_auction_block(text: &str, kind: AbilityKind) -> Option<AbilityDefinition> {
    let (rest, participants) = parse_participants_opener(text)?;
    let (rest, starting_bid) = parse_starting_bid_line(rest)?;
    let (rest, ()) = parse_auction_procedure(rest)?;
    let payoff = parse_auction_payoff(rest, kind)?;
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
    (
        tag_no_case::<_, _, OracleError<'_>>("you start the bidding with a bid of "),
        alt((
            value(
                LifeAuctionStartingBid::ControllerChooses,
                tag_no_case::<_, _, OracleError<'_>>("any number"),
            ),
            map(parse_number, LifeAuctionStartingBid::Fixed),
        )),
        opt(preceded(tag_no_case("."), tag_no_case(" "))),
    )
        .parse(i)
        .ok()
        .map(|(rest, (_, bid, _))| (rest, bid))
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
    let (text, _) = multispace0::<_, OracleError<'_>>(text).ok()?;
    if text.is_empty() {
        return None;
    }
    parse_auction_payoff_nom(text, kind).or_else(|| {
        let parsed = parse_effect_chain_with_context(text, kind, &mut ParseContext::default());
        if matches!(*parsed.effect, Effect::Unimplemented { .. }) {
            None
        } else {
            Some(normalize_payoff(parsed))
        }
    })
}

fn parse_auction_payoff_nom(text: &str, kind: AbilityKind) -> Option<AbilityDefinition> {
    let lower = text.to_ascii_lowercase();
    let (rest, mut chain) = parse_high_bidder_lose_life_head(text, &lower, kind)?;
    let (rest, _) = multispace0::<_, OracleError<'_>>(rest.as_str()).ok()?;
    if rest.is_empty() {
        return Some(chain);
    }

    let (rest, _) = opt(preceded(
        tag_no_case::<_, _, OracleError<'_>>("."),
        tag_no_case(" "),
    ))
    .parse(rest)
    .ok()?;
    if rest.is_empty() {
        return Some(chain);
    }

    let (condition, body) = strip_auction_winner_conditional(rest);
    if let Some(condition) = condition {
        let tail = parse_effect_chain_with_context(&body, kind, &mut ParseContext::default());
        if matches!(*tail.effect, Effect::Unimplemented { .. }) {
            return None;
        }
        chain = chain.sub_ability(normalize_payoff(tail).condition(condition));
        return Some(chain);
    }

    None
}

fn parse_high_bidder_lose_life_head(
    text: &str,
    lower: &str,
    kind: AbilityKind,
) -> Option<(String, AbilityDefinition)> {
    nom_on_lower(text, lower, |input| {
        let (rest, _) = tag("the high bidder loses life equal to the high bid").parse(input)?;
        let (rest, suffix) =
            opt(|input| parse_auction_payoff_conjunction(input, kind)).parse(rest)?;
        let (rest, _) = opt(tag(".")).parse(rest)?;
        let mut chain = build_lose_life_winning_bidder(kind);
        if let Some(suffix) = suffix {
            chain = chain.sub_ability(suffix);
        }
        Ok((rest, chain))
    })
    .map(|(chain, rest)| (rest.to_string(), chain))
}

fn parse_auction_payoff_conjunction(
    input: &str,
    kind: AbilityKind,
) -> nom::IResult<&str, AbilityDefinition> {
    preceded(
        tag(" and "),
        alt((
            map(
                preceded(tag("draws "), terminated(parse_number, tag(" cards"))),
                move |count| {
                    AbilityDefinition::new(
                        kind,
                        Effect::Draw {
                            count: QuantityExpr::Fixed {
                                value: count as i32,
                            },
                            target: TargetFilter::WinningBidder,
                        },
                    )
                },
            ),
            map(tag("gains control of the creature"), move |_| {
                AbilityDefinition::new(
                    kind,
                    Effect::GiveControl {
                        target: TargetFilter::ParentTarget,
                        recipient: TargetFilter::WinningBidder,
                    },
                )
            }),
        )),
    )
    .parse(input)
}

fn build_lose_life_winning_bidder(kind: AbilityKind) -> AbilityDefinition {
    AbilityDefinition::new(
        kind,
        Effect::LoseLife {
            amount: QuantityExpr::Ref {
                qty: QuantityRef::WinningBidAmount,
            },
            target: Some(TargetFilter::WinningBidder),
        },
    )
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
    use crate::types::ability::AbilityCondition;

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
        let sub = def.sub_ability.expect("payoff");
        assert!(matches!(*sub.effect, Effect::LoseLife { .. }));
        let give = sub.sub_ability.expect("give control");
        assert!(matches!(
            *give.effect,
            Effect::GiveControl {
                target: TargetFilter::ParentTarget,
                recipient: TargetFilter::WinningBidder,
            }
        ));
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
        let sub = def.sub_ability.expect("payoff");
        assert!(matches!(*sub.effect, Effect::LoseLife { .. }));
        let draw = sub.sub_ability.expect("draw");
        assert!(matches!(
            *draw.effect,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 4 },
                target: TargetFilter::WinningBidder,
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
        let sub = def.sub_ability.expect("payoff");
        assert!(matches!(*sub.effect, Effect::LoseLife { .. }));
        let counter = sub.sub_ability.expect("conditional counter");
        assert!(matches!(
            counter.condition,
            Some(AbilityCondition::AuctionWinnerIs {
                player: ControllerRef::You
            })
        ));
        assert!(matches!(*counter.effect, Effect::Counter { .. }));
    }
}
