use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;

use serde::de;
use serde::ser::SerializeStructVariant;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

use super::card::{PrintedCardRef, TokenImageRef};
use super::card_type::{CardType, CoreType, SubtypeSet, Supertype};
use super::counter::{CounterMatch, CounterType};
use super::events::BendingType;
use super::game_state::{
    is_zero_usize, DistributionUnit, LKISnapshot, MayTriggerOrigin, RetargetScope,
    TargetSelectionConstraint,
};
use super::identifiers::{CardId, ObjectId, TrackedSetId};
use super::keywords::{Keyword, KeywordKind};
use super::mana::{
    AbilityActivationScope, ManaColor, ManaCost, ManaType, SpellCostCriterion, ZoneSpend,
};
use super::phase::Phase;
use super::player::{PlayerCounterKind, PlayerId};
use super::proposed_event::AppliedReplacementKey;
use super::replacements::ReplacementEvent;
use super::statics::{ActivationExemption, CastFrequency, StaticMode};
use super::stickers::{AppliedSticker, StickerKind};
use super::triggers::TriggerMode;
use super::zones::{EtbTapState, Zone};
use crate::game::game_object::DisplaySource;
use crate::types::events::{ClashResult, PlayerActionKind};

// ---------------------------------------------------------------------------
// Supporting types
// ---------------------------------------------------------------------------

/// CR 608.2c-e: Who makes a choice during an effect's resolution (controller by
/// default per CR 608.2c; opponent/APNAP ordering per CR 608.2e). Not CR 700.2 —
/// that defines *modal* choices (picking a bulleted mode), a distinct concept.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Chooser {
    /// The controller of the spell/ability makes the choice.
    #[default]
    Controller,
    /// An opponent of the controller makes the choice (CR 608.2e).
    /// In 2-player, the single opponent. In multiplayer, controller chooses which opponent.
    Opponent,
}

/// CR 400.1 + CR 608.2c: Which player's zone supplies cards for a direct
/// zone choice during resolution.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ZoneOwner {
    /// The controller of the spell/ability owns the referenced zone.
    #[default]
    Controller,
    /// The first player target of the resolving spell/ability owns the referenced zone.
    TargetedPlayer,
    /// An opponent of the controller owns the referenced zone.
    Opponent,
    /// CR 701.38d: The scoped player (voter) owns the referenced zone.
    /// Used by per-ballot vote iteration (Expropriate) where the candidate
    /// pool is "permanents owned by the voter". For Battlefield, filters
    /// by `obj.owner` (ownership) rather than `obj.controller` (control).
    ScopedPlayer,
    /// CR 101.4 + CR 608.2c: Every player owns a referenced zone, iterated in
    /// APNAP order. A `ChooseFromZone { zone_owner: EachPlayer }` parks one
    /// choice per player, drawing candidates from THAT player's zone, and
    /// accumulates each pick into the resolution chain's tracked object set.
    /// Building block for Breach the Multiverse ("For each player, choose a
    /// creature or planeswalker card in that player's graveyard").
    EachPlayer,
    /// CR 101.4 + CR 102.2 + CR 608.2c: Every OPPONENT of the controller owns a
    /// referenced zone, iterated in APNAP order (the controller is excluded).
    /// The each-opponent leaf of the per-player iteration axis — same
    /// accumulate-into-tracked-set machinery as [`ZoneOwner::EachPlayer`], but
    /// the controller's own permanents are never offered. Building block for
    /// Kaya, Spirits' Justice's −2 ("For each other player, exile up to one
    /// target creature that player controls").
    EachOpponent,
    /// CR 400.1 + CR 607.2a: The referenced zone is scanned across ALL owners —
    /// ownership imposes no restriction, and the effect's `TargetFilter`
    /// performs every bit of scoping. Used when the candidate pool's membership
    /// is defined by something other than ownership, so owner-gating would
    /// wrongly drop valid cards. The motivating case is "a creature card exiled
    /// with [this source]" (Koh, the Face Stealer): exile is a zone shared by
    /// all players (CR 400.1), and "exiled with [source]" is a linked-ability
    /// reference (CR 607.2a) whose membership is the source's linked-exile set
    /// regardless of who owns each card — and Koh typically exiles *opponents'*
    /// creatures, so gating to the controller's own exiled cards empties the
    /// pool. This is the single-pool owner-agnostic leaf of the scope axis:
    /// distinct from [`ZoneOwner::EachPlayer`]/[`ZoneOwner::EachOpponent`],
    /// which iterate one prompt per player and accumulate into a tracked set —
    /// this raises ONE prompt over the whole-zone union. Mirrors the
    /// `ScopedPlayer`-on-Battlefield branch in `collect_direct_zone_cards`
    /// (scan broadly, let the filter narrow), but with no owner restriction at
    /// all.
    AllOwners,
}

/// CR 105.1 + CR 205.2: A fixed, closed enumeration whose members an effect
/// iterates over once each ("for each color, …", "for each card type, …").
///
/// This is the *category* axis of for-each iteration — distinct from per-object
/// iteration ("for each creature you control", which counts battlefield
/// objects). The members come from a printed rules enumeration, not from game
/// state: the five colors (CR 105.1) or the card types that can appear on cards
/// in a library (CR 205.2a).
///
/// During resolution the iterating effect binds each member in turn and the
/// per-member payload references it ("a card of *that* color/type") by
/// augmenting its candidate filter with the bound member's
/// `FilterProp::HasColor` / `TypeFilter`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum IterationCategory {
    /// CR 105.1: The five colors, iterated in WUBRG order.
    Color,
    /// CR 205.2a: The card types that can appear on a card in a library —
    /// artifact, battle, creature, enchantment, instant, kindred, land,
    /// planeswalker, and sorcery — iterated in CR 205.2a order.
    CardType,
}

impl IterationCategory {
    /// CR 105.1 + CR 205.2a: The ordered member filters for this category, one
    /// per member. Each entry pairs the bound member with a `TargetFilter`
    /// restricting candidates to objects that *are* that member (a color via
    /// `FilterProp::HasColor`, a card type via `TypeFilter`). Used by the
    /// for-each-category iterator to drive one prompt per member.
    pub fn member_filters(self) -> Vec<TargetFilter> {
        match self {
            // CR 105.1: WUBRG order.
            IterationCategory::Color => ManaColor::ALL
                .iter()
                .map(|&color| {
                    TargetFilter::Typed(TypedFilter {
                        type_filters: vec![],
                        controller: None,
                        properties: vec![FilterProp::HasColor { color }],
                    })
                })
                .collect(),
            // The members are the CR 205.2a card types that can appear in a
            // library, offered in CR 205.2a order: artifact, battle, creature,
            // enchantment, instant, kindred, land, planeswalker, and sorcery.
            // (Author's gloss, not quoted CR text:) the nontraditional
            // command-zone types — dungeon/plane/phenomenon/scheme/conspiracy/
            // vanguard — can never be in a library, so they are never offered.
            //
            // Per CR 308.1 ("each kindred card has another card type"), a kindred
            // card is exilable at BOTH the Kindred member and its other-type
            // member (e.g. a Kindred Sorcery matches both the kindred member and
            // the sorcery member). Kindred is offered explicitly via the
            // dedicated `TypeFilter::Kindred` member below.
            IterationCategory::CardType => [
                TypeFilter::Artifact,
                TypeFilter::Battle,
                TypeFilter::Creature,
                TypeFilter::Enchantment,
                TypeFilter::Instant,
                TypeFilter::Kindred,
                TypeFilter::Land,
                TypeFilter::Planeswalker,
                TypeFilter::Sorcery,
            ]
            .into_iter()
            .map(|type_filter| {
                TargetFilter::Typed(TypedFilter {
                    type_filters: vec![type_filter],
                    controller: None,
                    properties: vec![],
                })
            })
            .collect(),
        }
    }
}

/// CR 608.2c: Per-member terminal action during [`Effect::ForEachCategory`]
/// iteration. The category axis (`IterationCategory`) is shared; only the
/// per-member body differs (exile from tracked pool vs put counter on permanent).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ForEachCategoryAction {
    /// CR 608.2c + CR 105.1 / CR 205.2a: Exile one card of the bound member from
    /// the chain tracked pool — "for each color, you may exile a card of that
    /// color from among the revealed cards" (Sanar).
    ExileFromPool {
        /// The zone the pool cards live in (where each chosen card is exiled from).
        zone: Zone,
        /// CR 608.2d: When true (the "you may exile" idiom), each member's pick
        /// is a resolution-time optional choice (0..=1 per member).
        #[serde(default = "default_true")]
        up_to: bool,
    },
    /// CR 608.2c + CR 105.1 / CR 122.1: Put counters on one battlefield permanent
    /// matching a base filter augmented with the bound member — "for each color,
    /// put a +1/+1 counter on a Dragon you control of that color" (Call the Spirit
    /// Dragons).
    PutCounter {
        /// Base permanent filter before the per-member color/type restriction.
        target: TargetFilter,
        /// CR 122.1: Counter type placed on each chosen permanent.
        counter_type: crate::types::counter::CounterType,
        /// CR 122.1: Number of counters placed per member iteration.
        count: QuantityExpr,
    },
}

/// CR 101.4: Who selects permanents in a multi-player category choice effect
/// (e.g., Cataclysm, Tragic Arrogance). Determines whether each player independently
/// chooses which of their permanents to keep, or the spell's controller decides for everyone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CategoryChooserScope {
    /// Each player chooses their own permanents in APNAP order (Cataclysm, Cataclysmic Gearhulk).
    #[default]
    EachPlayerSelf,
    /// The controller of the spell/ability chooses for all players (Tragic Arrogance).
    ControllerForAll,
}

/// CR 101.4 + CR 701.21a: Constraint on the permanents a player protects from
/// a "choose … and sacrifice the rest" instruction.
///
/// This is deliberately separate from the legacy category and total-power
/// fields on [`Effect::ChooseAndSacrificeRest`]. Those fields remain readable
/// for existing card data and saved games; new cardinality-only wordings use a
/// typed constraint rather than a one-off boolean or card-name branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum KeeperConstraint {
    /// The chooser must protect exactly this many eligible permanents. If fewer
    /// are eligible, the instruction does as much as possible and protects all
    /// of them (CR 609.3).
    ExactCount { count: QuantityExpr },
}

/// Additional selection constraints for tracked-set card picks during resolution.
///
/// Internally tagged (`{ "type": "DistinctCardTypes", "categories": [...] }`) to
/// match the sibling [`SearchSelectionConstraint`] convention and the frontend's
/// `ChooseFromZoneConstraint` type, which discriminates on the `type` field. The
/// `CardChoiceModal` confirm gate reads `constraint.type`; without the tag the
/// default external representation (`{ "DistinctCardTypes": {...} }`) leaves
/// `type` undefined and the modal can never validate a selection.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ChooseFromZoneConstraint {
    /// The chosen cards must admit an injective assignment to distinct card types
    /// from the listed categories.
    DistinctCardTypes { categories: Vec<CoreType> },
}

/// Selection constraint applied to multi-card library searches at the
/// `WaitingFor::SearchChoice` step. Lives one abstraction up from `count` /
/// `up_to` so the engine can reject illegal combinations and the AI can
/// prune its candidate space without bespoke per-card knowledge.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SearchSelectionConstraint {
    /// CR 107.1c: No restriction beyond `count` (and `up_to`).
    #[default]
    None,
    /// CR 608.2c: The spell's printed text constrains the selected set by
    /// object qualities — e.g. "with different names", "with different
    /// powers", or "that don't share a mana value, power, toughness, or card
    /// type with each other".
    DistinctQualities { qualities: Vec<SharedQuality> },
    /// CR 608.2c + CR 202.3: The chosen set's combined mana value must satisfy
    /// the printed comparator. Used by "cards with total mana value N or less"
    /// tutor text, where the constraint applies across the selected set rather
    /// than to each individual card.
    TotalManaValue { comparator: Comparator, value: i32 },
    /// CR 701.23a + CR 701.23h: A single library search may ask for several
    /// independently described cards ("a black card, a green card, and a blue
    /// card"). The chosen set must be assignable to the printed descriptions,
    /// with each physical card used for at most one description slot.
    MatchEachFilter { filters: Vec<TargetFilter> },
}

impl SearchSelectionConstraint {
    /// CR 701.23b vs CR 701.23d: a *stated-quality* search (any constrained
    /// variant) may find fewer cards than requested — including none. A pure
    /// *quantity* search (`None`) must find as many as possible. Drives the
    /// SearchChoice lower bound in the submission guard and AI candidate gen.
    pub fn permits_partial_find(&self) -> bool {
        !matches!(self, SearchSelectionConstraint::None)
    }
}

/// CR 400.11 + CR 406.3: Candidate pool for outside-game searches. The
/// baseline pool is the player's sideboard; Karn/Coax-class text widens that
/// pool to include owned face-up exile cards that match the same filter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum OutsideGameSourcePool {
    /// CR 400.11a: Tournament sideboard / casual outside-the-game collection.
    #[default]
    Sideboard,
    /// CR 400.11a + CR 406.3: Sideboard plus matching owned face-up exile.
    SideboardAndFaceUpExile,
}

impl OutsideGameSourcePool {
    pub fn includes_face_up_exile(self) -> bool {
        matches!(self, OutsideGameSourcePool::SideboardAndFaceUpExile)
    }
}

/// CR 701.23a + CR 608.2c: A search whose found set is partitioned between two
/// destinations — e.g. Cultivate ("put one onto the battlefield tapped and the
/// other into your hand"). `primary_count` cards go to `primary_destination`
/// (the searcher's choice when more than `primary_count` are found); the rest go
/// to `rest_destination`. `primary_destination` is `Battlefield` for the A/B/C
/// cluster, where `primary_enter_tapped` routes the entry through the ETB
/// pipeline so the permanent enters tapped (CR 614.1 / CR 110.5b).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchDestinationSplit {
    /// Where the chosen primary cards go (Battlefield for cultivate-class).
    pub primary_destination: Zone,
    /// How many of the found cards go to `primary_destination` (literal N from
    /// "put N ..."). Mirrors `Effect::Dig.keep_count`.
    pub primary_count: u32,
    /// CR 614.1 / CR 110.5b: Primary cards enter the battlefield tapped.
    /// Mirrors `Effect::ChangeZone.enter_tapped`.
    #[serde(
        default,
        with = "super::zones::etb_tap_bool_compat",
        skip_serializing_if = "EtbTapState::is_unspecified"
    )]
    pub primary_enter_tapped: EtbTapState,
    /// Where the remaining found cards go ("the rest"/"the other" — Hand for
    /// cultivate-class).
    pub rest_destination: Zone,
}

/// CR 608.2d: Who may choose to perform an optional effect during resolution.
/// Used with `AbilityDefinition::optional_for` to route the "you may" prompt to opponents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OpponentMayScope {
    /// "any opponent may" — each opponent in APNAP order gets the chance; first accept wins.
    AnyOpponent,
    /// CR 608.2d + CR 101.4: "any player may" — every player INCLUDING the controller is offered in APNAP order; first accept wins (distinct from AnyOpponent which excludes the controller).
    AnyPlayer,
    /// CR 608.2d + CR 101.4: "any other player may" — every player EXCEPT the
    /// affected player of the currently-resolving replaced event is offered in
    /// APNAP order; first accept wins. This is affected-player-relative, not
    /// controller-relative: for Zur's Weirding the excluded player is whoever
    /// would have drawn (the drain's stashed event target), which need not be
    /// the ability's controller. Distinct from `AnyOpponent` (excludes the
    /// controller) and `AnyPlayer` (excludes no one).
    AnyOtherPlayer,
}

/// CR 609.3 + CR 608.2d: whether a "choose a number" domain must exclude numbers
/// already committed on this source ("...that hasn't been chosen"), or repeats
/// are legal. Parse-detected; static; serialized only when non-default so the
/// existing `NumberRange` card-data stays byte-stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum NumberDistinctness {
    /// The default for the entire existing `NumberRange` pool — repeats allowed.
    #[default]
    Repeatable,
    /// Each commit must differ from every prior `ChosenAttribute::Number` on the
    /// source (The Toymaker's Trap "a number ... that hasn't been chosen").
    DistinctFromSourceHistory,
}

/// What kind of named choice the player must make at resolution time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChoiceType {
    /// CR 205.3m: A choice among creature types. `options`, when non-empty,
    /// narrows the offered set below the full creature-type list to an explicit
    /// Oracle-listed candidate set (e.g. A Killer Among Us' "secretly choose
    /// Human, Merfolk, or Goblin") — mirrors the `Color { excluded }` /
    /// `CardType { excluded }` restriction axis. Empty ⇒ all creature types
    /// (Morophon / Changeling), which keeps existing card-data JSON byte-stable.
    CreatureType {
        options: Vec<String>,
    },
    Color {
        /// Colors that cannot be chosen by this prompt.
        ///
        /// CR 105.1 + CR 105.4 define the five legal color choices; prompts such as
        /// "choose a color other than white" restrict that set.
        excluded: Vec<ManaColor>,
    },
    OddOrEven,
    BasicLandType,
    /// CR 205.2a: A choice among card types. `excluded` narrows the offered
    /// set below the full seven-type list (e.g. Archon of Valor's Reach
    /// excludes Creature and Land, leaving "artifact, enchantment, instant,
    /// sorcery, or planeswalker") — mirrors the `Color { excluded }` axis.
    CardType {
        excluded: Vec<CoreType>,
    },
    CardName,
    /// "Choose a number between X and Y" — generates string options "0", "1", ..., "Y".
    NumberRange {
        min: u8,
        max: u8,
        /// CR 609.3: distinctness requirement, parse-detected from "that hasn't
        /// been chosen". Default `Repeatable` for every existing card.
        distinctness: NumberDistinctness,
    },
    /// "Choose left or right", "choose fame or fortune" — options come from the parser.
    Labeled {
        options: Vec<String>,
    },
    /// "Choose a land type" — includes basic + common nonbasic land types.
    LandType,
    /// Choose a card predicate for resolution-scoped comparisons such as
    /// "card of the chosen kind". Unlike `Color`, this is not a persisted
    /// source choice; it classifies the card currently being revealed/checked.
    CardPredicate {
        options: Vec<CardPredicateChoice>,
    },
    /// Guess which card predicate the revealed card will match. Used by
    /// top-card "guessed right" sequences and intentionally resolution-scoped
    /// rather than persisted on the source card.
    CardPredicateGuess {
        options: Vec<CardPredicateChoice>,
    },
    /// "Choose an opponent" — selects one opponent player (CR 102.3 defines an
    /// opponent as any player not on the choosing player's team).
    ///
    /// `restriction`, when present, narrows the eligible opponents to those
    /// matching the embedded `PlayerFilter` (CR 102.3 + CR 608.2d). This keeps
    /// "choose an opponent with the most life among your opponents" (The Master,
    /// Gallifrey's End) a genuine single-pick step — the controller picks ONE of
    /// the qualifying opponents (CR 608.2d handles ties) — rather than fanning
    /// the effect out to every tied opponent. Boxed to avoid inflating the
    /// `ChoiceType` enum with the recursive `PlayerFilter` payload.
    Opponent {
        restriction: Option<Box<PlayerFilter>>,
    },
    /// "Choose a player" — selects any player in the game.
    Player,
    /// "Choose two colors" — selects two distinct mana colors.
    TwoColors,
    /// "Choose a word" — names any English word (Un-set and silver-border cards).
    Word,
    /// "Choose an artist" — selects a Magic card artist name.
    Artist,
    /// CR 608.2d: "Choose [an ability from this list]" — the option set is a
    /// typed list of `Keyword`s, not free-form strings. Used by Urborg /
    /// Walking Sponge / Phyrexian Splicer ("target creature loses first strike
    /// or swampwalk until end of turn"). The chosen keyword persists as
    /// `ChosenAttribute::Keyword` on the source so a downstream
    /// `ContinuousModification::RemoveChosenKeyword` (Layer 6) can strip the
    /// chosen ability at layer-evaluation time.
    ///
    /// `count` is the "how many to choose" leaf parameterization on this same
    /// CR 608.2d choice axis (NOT a separate `TwoKeywords` variant): `count: 1`
    /// is the bare "choose an ability" case (Urborg), `count: 2` is Greymond's
    /// "choose two abilities from among first strike, vigilance, and lifelink".
    /// For `count > 1` each chosen keyword is persisted as its own
    /// `ChosenAttribute::Keyword`.
    Keyword {
        options: Vec<Keyword>,
        count: usize,
    },
    /// CR 608.2d + CR 122.1: "choose a counter on it" — pick one of the distinct
    /// counter kinds currently present on a specific object (The Caves of
    /// Androzani II/III). The concrete `options` list is enumerated at
    /// resolution from the target object's counters (mirroring the runtime-baked
    /// `Keyword { options }` template idiom), so the same interactive
    /// `WaitingFor::NamedChoice` seam renders it. The chosen kind persists as
    /// `ChosenAttribute::Counter` on the source for a downstream
    /// `Effect::PutChosenCounter` ("put an additional counter of that kind").
    CounterKind {
        options: Vec<CounterType>,
    },
}

/// A predicate option that can be chosen or guessed for a card revealed during
/// the current resolution. Options may overlap: a red-black card matches both
/// `Color(Red)` and `Color(Black)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CardPredicateChoice {
    Land,
    Nonland,
    Color(ManaColor),
}

impl CardPredicateChoice {
    pub fn label(self) -> String {
        match self {
            Self::Land => "Land".to_string(),
            Self::Nonland => "Nonland".to_string(),
            Self::Color(color) => format!("{color:?}"),
        }
    }

    pub fn from_label(label: &str) -> Option<Self> {
        match label {
            "Land" => Some(Self::Land),
            "Nonland" => Some(Self::Nonland),
            _ => label.parse::<ManaColor>().ok().map(Self::Color),
        }
    }

    pub fn matches_card(self, core_types: &[CoreType], colors: &[ManaColor]) -> bool {
        match self {
            Self::Land => core_types.contains(&CoreType::Land),
            Self::Nonland => !core_types.contains(&CoreType::Land),
            Self::Color(color) => colors.contains(&color),
        }
    }
}

impl ChoiceType {
    /// Unrestricted creature-type choice (all creature types offered).
    pub fn creature_type() -> Self {
        Self::CreatureType {
            options: Vec::new(),
        }
    }

    /// Creature-type choice restricted to an explicit Oracle-listed candidate
    /// set (CR 205.3m), e.g. "secretly choose Human, Merfolk, or Goblin".
    pub fn creature_type_from(options: Vec<String>) -> Self {
        Self::CreatureType { options }
    }

    pub fn color() -> Self {
        Self::Color {
            excluded: Vec::new(),
        }
    }

    pub fn color_excluding(excluded: Vec<ManaColor>) -> Self {
        Self::Color { excluded }
    }

    pub fn card_type() -> Self {
        Self::CardType {
            excluded: Vec::new(),
        }
    }

    pub fn card_type_excluding(excluded: Vec<CoreType>) -> Self {
        Self::CardType { excluded }
    }

    pub fn land_or_nonland_card_predicate_options() -> Vec<CardPredicateChoice> {
        vec![CardPredicateChoice::Land, CardPredicateChoice::Nonland]
    }

    pub fn is_resolution_scoped_card_predicate_choice(&self) -> bool {
        matches!(
            self,
            Self::CardPredicate { .. } | Self::CardPredicateGuess { .. }
        )
    }

    pub fn is_card_predicate_guess(&self) -> bool {
        matches!(self, Self::CardPredicateGuess { .. })
    }

    pub fn needs_choice_source_context(&self) -> bool {
        matches!(self, Self::CardPredicateGuess { .. })
    }

    pub fn card_predicate_labels(options: &[CardPredicateChoice]) -> Vec<String> {
        options.iter().map(|option| option.label()).collect()
    }

    /// Whether the player supplies the chosen value at runtime rather than the
    /// engine enumerating a fixed option set.
    ///
    /// `CardName` options come from the frontend's local card database (the
    /// engine sends an empty list to avoid serializing 30k+ names) and are
    /// wired end-to-end via the free-text name search. `Word` / `Artist` are
    /// likewise player-supplied free-text in principle, but their free-text
    /// frontend/legal-action path is not yet implemented (only `CardName` is
    /// synthesized by `named_choice_actions` and given a text input by
    /// `NamedChoiceModal`) — a separate known gap. They are kept here so an
    /// empty engine list for them is treated as a still-to-be-supplied value
    /// rather than silently skipped as impossible. For every other choice type
    /// the engine fully enumerates the legal options, so an empty option list
    /// means there is genuinely nothing to choose.
    ///
    /// Used to distinguish a legitimately-empty engine option list (this
    /// predicate is true) from an impossible choice that must resolve as a
    /// no-op per CR 609.3 (this predicate is false).
    pub fn options_supplied_by_player(&self) -> bool {
        matches!(self, Self::CardName | Self::Word | Self::Artist)
    }
}

impl Serialize for ChoiceType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            // Serialize the unrestricted form as the legacy unit variant
            // "CreatureType" so existing Morophon/Changeling card-data JSON
            // stays byte-stable; only emit the struct form when a candidate
            // restriction is present.
            Self::CreatureType { options } => {
                if options.is_empty() {
                    serializer.serialize_unit_variant("ChoiceType", 0, "CreatureType")
                } else {
                    let mut variant =
                        serializer.serialize_struct_variant("ChoiceType", 0, "CreatureType", 1)?;
                    variant.serialize_field("options", options)?;
                    variant.end()
                }
            }
            Self::Color { excluded } => {
                if excluded.is_empty() {
                    serializer.serialize_unit_variant("ChoiceType", 1, "Color")
                } else {
                    let mut variant =
                        serializer.serialize_struct_variant("ChoiceType", 1, "Color", 1)?;
                    variant.serialize_field("excluded", excluded)?;
                    variant.end()
                }
            }
            Self::OddOrEven => serializer.serialize_unit_variant("ChoiceType", 2, "OddOrEven"),
            Self::BasicLandType => {
                serializer.serialize_unit_variant("ChoiceType", 3, "BasicLandType")
            }
            // Serialize the unrestricted form as the legacy unit variant
            // "CardType" so existing card-data JSON stays byte-stable; only
            // emit the struct form when a restriction is present.
            Self::CardType { excluded } => {
                if excluded.is_empty() {
                    serializer.serialize_unit_variant("ChoiceType", 4, "CardType")
                } else {
                    let mut variant =
                        serializer.serialize_struct_variant("ChoiceType", 4, "CardType", 1)?;
                    variant.serialize_field("excluded", excluded)?;
                    variant.end()
                }
            }
            Self::CardName => serializer.serialize_unit_variant("ChoiceType", 5, "CardName"),
            Self::NumberRange {
                min,
                max,
                distinctness,
            } => {
                // Emit `distinctness` only when non-default so existing
                // `{min,max}` card-data stays byte-stable.
                let field_count = 2 + (*distinctness != NumberDistinctness::Repeatable) as usize;
                let mut variant = serializer.serialize_struct_variant(
                    "ChoiceType",
                    6,
                    "NumberRange",
                    field_count,
                )?;
                variant.serialize_field("min", min)?;
                variant.serialize_field("max", max)?;
                if *distinctness != NumberDistinctness::Repeatable {
                    variant.serialize_field("distinctness", distinctness)?;
                }
                variant.end()
            }
            Self::Labeled { options } => {
                let mut variant =
                    serializer.serialize_struct_variant("ChoiceType", 7, "Labeled", 1)?;
                variant.serialize_field("options", options)?;
                variant.end()
            }
            Self::LandType => serializer.serialize_unit_variant("ChoiceType", 8, "LandType"),
            Self::CardPredicate { options } => {
                let mut variant =
                    serializer.serialize_struct_variant("ChoiceType", 15, "CardPredicate", 1)?;
                variant.serialize_field("options", options)?;
                variant.end()
            }
            Self::CardPredicateGuess { options } => {
                let mut variant = serializer.serialize_struct_variant(
                    "ChoiceType",
                    16,
                    "CardPredicateGuess",
                    1,
                )?;
                variant.serialize_field("options", options)?;
                variant.end()
            }
            // Serialize the unrestricted form as the legacy unit variant
            // "Opponent" so existing card-data JSON stays byte-stable; only emit
            // the struct form when a restriction is present.
            Self::Opponent { restriction } => match restriction {
                None => serializer.serialize_unit_variant("ChoiceType", 9, "Opponent"),
                Some(restriction) => {
                    let mut variant =
                        serializer.serialize_struct_variant("ChoiceType", 9, "Opponent", 1)?;
                    variant.serialize_field("restriction", restriction)?;
                    variant.end()
                }
            },
            Self::Player => serializer.serialize_unit_variant("ChoiceType", 10, "Player"),
            Self::TwoColors => serializer.serialize_unit_variant("ChoiceType", 11, "TwoColors"),
            Self::Word => serializer.serialize_unit_variant("ChoiceType", 12, "Word"),
            Self::Artist => serializer.serialize_unit_variant("ChoiceType", 13, "Artist"),
            Self::Keyword { options, count } => {
                // Skip `count` when it is the default (1) so existing
                // single-keyword card-data JSON stays byte-stable; only emit
                // the field for multi-choice cards (Greymond, count: 2).
                if *count == 1 {
                    let mut variant =
                        serializer.serialize_struct_variant("ChoiceType", 14, "Keyword", 1)?;
                    variant.serialize_field("options", options)?;
                    variant.end()
                } else {
                    let mut variant =
                        serializer.serialize_struct_variant("ChoiceType", 14, "Keyword", 2)?;
                    variant.serialize_field("options", options)?;
                    variant.serialize_field("count", count)?;
                    variant.end()
                }
            }
            // CR 608.2d + CR 122.1: Runtime-only counter-kind choice (never
            // appears in card data; baked with concrete kinds at resolution).
            Self::CounterKind { options } => {
                let mut variant =
                    serializer.serialize_struct_variant("ChoiceType", 15, "CounterKind", 1)?;
                variant.serialize_field("options", options)?;
                variant.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for ChoiceType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum ChoiceTypeRepr {
            Unit(String),
            Data(ChoiceTypeData),
        }

        #[derive(Deserialize)]
        enum ChoiceTypeData {
            CreatureType {
                #[serde(default)]
                options: Vec<String>,
            },
            Color {
                #[serde(default)]
                excluded: Vec<ManaColor>,
            },
            CardType {
                #[serde(default)]
                excluded: Vec<CoreType>,
            },
            NumberRange {
                min: u8,
                max: u8,
                #[serde(default)]
                distinctness: NumberDistinctness,
            },
            Labeled {
                options: Vec<String>,
            },
            CardPredicate {
                options: Vec<CardPredicateChoice>,
            },
            CardPredicateGuess {
                options: Vec<CardPredicateChoice>,
            },
            Opponent {
                #[serde(default)]
                restriction: Option<Box<PlayerFilter>>,
            },
            Keyword {
                options: Vec<Keyword>,
                // Default to 1 (the bare single-keyword choice) so legacy
                // `Keyword { options }` JSON round-trips to `count: 1`.
                #[serde(default = "default_keyword_choice_count")]
                count: usize,
            },
            CounterKind {
                options: Vec<CounterType>,
            },
        }

        fn default_keyword_choice_count() -> usize {
            1
        }

        match ChoiceTypeRepr::deserialize(deserializer)? {
            ChoiceTypeRepr::Unit(value) => match value.as_str() {
                "CreatureType" => Ok(Self::creature_type()),
                "Color" => Ok(Self::color()),
                "OddOrEven" => Ok(Self::OddOrEven),
                "BasicLandType" => Ok(Self::BasicLandType),
                "CardType" => Ok(Self::card_type()),
                "CardName" => Ok(Self::CardName),
                "LandType" => Ok(Self::LandType),
                "Opponent" => Ok(Self::Opponent { restriction: None }),
                "Player" => Ok(Self::Player),
                "TwoColors" => Ok(Self::TwoColors),
                "Word" => Ok(Self::Word),
                "Artist" => Ok(Self::Artist),
                other => Err(de::Error::unknown_variant(
                    other,
                    &[
                        "CreatureType",
                        "Color",
                        "OddOrEven",
                        "BasicLandType",
                        "CardType",
                        "CardName",
                        "LandType",
                        "Opponent",
                        "Player",
                        "TwoColors",
                        "Word",
                        "Artist",
                    ],
                )),
            },
            ChoiceTypeRepr::Data(data) => match data {
                ChoiceTypeData::CreatureType { options } => Ok(Self::CreatureType { options }),
                ChoiceTypeData::Color { excluded } => Ok(Self::Color { excluded }),
                ChoiceTypeData::CardType { excluded } => Ok(Self::CardType { excluded }),
                ChoiceTypeData::NumberRange {
                    min,
                    max,
                    distinctness,
                } => Ok(Self::NumberRange {
                    min,
                    max,
                    distinctness,
                }),
                ChoiceTypeData::Labeled { options } => Ok(Self::Labeled { options }),
                ChoiceTypeData::CardPredicate { options } => Ok(Self::CardPredicate { options }),
                ChoiceTypeData::CardPredicateGuess { options } => {
                    Ok(Self::CardPredicateGuess { options })
                }
                ChoiceTypeData::Opponent { restriction } => Ok(Self::Opponent { restriction }),
                ChoiceTypeData::Keyword { options, count } => Ok(Self::Keyword { options, count }),
                ChoiceTypeData::CounterKind { options } => Ok(Self::CounterKind { options }),
            },
        }
    }
}

/// The five basic land types (CR 305.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BasicLandType {
    Plains,
    Island,
    Swamp,
    Mountain,
    Forest,
}

impl BasicLandType {
    /// The corresponding mana color for this basic land type.
    pub fn mana_color(self) -> ManaColor {
        match self {
            Self::Plains => ManaColor::White,
            Self::Island => ManaColor::Blue,
            Self::Swamp => ManaColor::Black,
            Self::Mountain => ManaColor::Red,
            Self::Forest => ManaColor::Green,
        }
    }

    /// All five basic land types in WUBRG order (CR 305.6).
    pub fn all() -> &'static [BasicLandType] {
        &[
            Self::Plains,
            Self::Island,
            Self::Swamp,
            Self::Mountain,
            Self::Forest,
        ]
    }

    /// The subtype string as it appears in card type lines.
    pub fn as_subtype_str(&self) -> &'static str {
        match self {
            Self::Plains => "Plains",
            Self::Island => "Island",
            Self::Swamp => "Swamp",
            Self::Mountain => "Mountain",
            Self::Forest => "Forest",
        }
    }
}

impl std::str::FromStr for BasicLandType {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        match s {
            "Plains" => Ok(Self::Plains),
            "Island" => Ok(Self::Island),
            "Swamp" => Ok(Self::Swamp),
            "Mountain" => Ok(Self::Mountain),
            "Forest" => Ok(Self::Forest),
            _ => Err(()),
        }
    }
}

/// Odd or even — used by cards like "choose odd or even."
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Parity {
    Odd,
    Even,
}

/// Source for odd/even parity predicates over mana value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum ParitySource {
    /// A fixed printed odd/even quality.
    Fixed(Parity),
    /// CR 608.2c: Reads the most recent odd/even named choice made earlier in
    /// the same resolving instruction sequence.
    LastNamedChoice,
}

/// A branch in a d20/d6/d4 result table (CR 706.2).
/// Each branch covers a contiguous range of die results and maps to an effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DieResultBranch {
    pub min: u8,
    pub max: u8,
    pub effect: Box<AbilityDefinition>,
}

/// CR 706.2: Modifier applied to a die roll's natural result before the
/// effect's result table is consulted. "Roll a d20 and add the number of
/// cards in your hand" → `Add(QuantityExpr::Ref(HandSize { player: Controller }))`.
/// "Roll a d20 and subtract the number of cards in your hand" → `Subtract(...)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DieRollModifier {
    /// Add the resolved quantity to the natural roll.
    Add { value: QuantityExpr },
    /// Subtract the resolved quantity from the natural roll.
    Subtract { value: QuantityExpr },
}

impl std::str::FromStr for Parity {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        match s {
            "Odd" => Ok(Self::Odd),
            "Even" => Ok(Self::Even),
            _ => Err(()),
        }
    }
}

/// CR 615: Damage prevention scope.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PreventionScope {
    /// Prevent all damage (combat + noncombat).
    #[default]
    AllDamage,
    /// Prevent only combat damage.
    CombatDamage,
}

/// CR 615: How much damage to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PreventionAmount {
    /// "Prevent the next N damage"
    Next(u32),
    /// "Prevent all damage"
    All,
    /// "Prevent all but N of that damage" (Temple Altisaur)
    AllBut(u32),
}

/// CR 614.9: Recipient of a one-shot damage-redirection effect — the
/// battle/creature/planeswalker/player the replaced damage is dealt to instead.
/// Resolved to a concrete object/player at effect-resolution time (see
/// `effects::create_damage_replacement::resolve`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DamageRedirectTarget {
    /// "...to you instead" — the replacement source's controller (Jade Monolith,
    /// Goblin Psychopath).
    Controller,
    /// "...to ~ instead" / "...dealt to this creature instead" — the replacement
    /// source object itself (Beacon of Destiny).
    SourceObject,
    /// "...to target creature instead" — an object chosen as a target of the
    /// creating ability (Soltari Guerrillas).
    ChosenObjectTarget,
}

/// Shield type for one-shot replacement effects that expire at cleanup.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ShieldKind {
    #[default]
    None,
    /// CR 701.19a: Regeneration shield — consumed on use, expires at cleanup.
    Regeneration,
    /// CR 615: Prevention shield — absorbs/prevents damage, expires at cleanup.
    Prevention { amount: PreventionAmount },
    /// CR 614.5 + CR 614.1a: One-shot damage-amount replacement created by an
    /// effect ("the next time ... would deal damage this turn, it deals double
    /// that damage instead" — Desperate Gambit). Gets one opportunity to affect
    /// a damage event, is consumed on use, and expires at cleanup. Distinct from
    /// a continuous static `damage_modification` (Furnace of Rath), which keeps
    /// `ShieldKind::None` and re-applies to every damage event.
    DamageReplacementOneShot,
    /// CR 614.9: One-shot redirection shield — replaces all or part of a damage
    /// event's recipient with `recipient`. `All` covers "the next time ... would
    /// deal damage"; `Next(n)` covers "the next N damage ... is dealt to ..."
    /// redirections. Consumed on use, expires at cleanup.
    Redirection {
        recipient: DamageRedirectTarget,
        amount: PreventionAmount,
    },
}

impl ShieldKind {
    pub fn is_none(&self) -> bool {
        matches!(self, ShieldKind::None)
    }

    pub fn is_shield(&self) -> bool {
        !self.is_none()
    }
}

/// CR 601.2 vs CR 305.1: Distinguishes "cast" (spells only) from "play" (spells + lands).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CardPlayMode {
    /// CR 601.2: Cast a spell (cannot play lands this way).
    #[default]
    Cast,
    /// CR 305.1: Play a card — cast if it's a spell, play as a land if it's a land.
    Play,
}

impl fmt::Display for CardPlayMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CardPlayMode::Cast => write!(f, "Cast"),
            CardPlayMode::Play => write!(f, "Play"),
        }
    }
}

impl std::str::FromStr for CardPlayMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "Cast" => Ok(CardPlayMode::Cast),
            "Play" => Ok(CardPlayMode::Play),
            _ => Err(format!("Unknown CardPlayMode: {s}")),
        }
    }
}

/// CR 608.2g vs CR 118.9: Which casting *mechanism* an `Effect::CastFromZone`
/// drives — i.e. *when* relative to the granting ability's resolution the card
/// is cast. This is orthogonal to `duration` (CR 611.2a permission-expiry),
/// `mode` (CR 601.2 cast vs CR 305.1 play), and `alt_ability_cost` (CR 118.9
/// cost-substitution); none of those describe the casting mechanism, so the
/// router must read this field rather than inferring the mechanism from them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CastFromZoneDriver {
    /// CR 118.9: Grant a lingering `CastingPermission` on the target card(s)
    /// and return — the controller casts the card later, during the granting
    /// effect's own (or a future) priority window. The default for every
    /// permission-granting cast-from-zone effect: Discover/Nashi/Jeleva-style
    /// "you may cast those exiled cards" and Rebound's next-upkeep recast offer
    /// (CR 702.88a), whose `duration: Some(UntilEndOfTurn)` then prunes the
    /// unused permission.
    #[default]
    LingeringPermission,
    /// CR 608.2g: Cast the card *as the granting ability resolves* — the card
    /// goes onto the stack immediately via `initiate_cast_during_resolution`,
    /// and the CR 608.2g timing bypass (sorcery-speed / empty-stack /
    /// active-player gates do not apply) is armed. Set by Suspend's
    /// last-time-counter trigger (CR 702.62a) so a suspended sorcery recast at
    /// upkeep is not blocked by the sorcery-speed gate (issue #1520).
    DuringResolution,
}

impl CastFromZoneDriver {
    /// Serde skip predicate — the `LingeringPermission` default is the common
    /// case and is elided from serialized `Effect::CastFromZone` bodies.
    pub fn is_default(&self) -> bool {
        matches!(self, CastFromZoneDriver::LingeringPermission)
    }

    /// CR 608.2g: true iff this effect casts the card as the granting ability
    /// resolves (Suspend's last-counter cast), rather than granting a lingering
    /// permission.
    pub fn is_during_resolution(&self) -> bool {
        matches!(self, CastFromZoneDriver::DuringResolution)
    }
}

/// CR 702.104a + CR 702.104b: The outcome of the Tribute choice the chosen opponent
/// made as the creature entered the battlefield. Persisted as a `ChosenAttribute` on
/// the Tribute creature so the companion "if tribute wasn't paid" trigger (CR
/// 702.104b) can read the decision. A typed enum rather than a `bool` so the absence
/// of any `TributeOutcome` remains distinguishable from an explicit `Declined`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TributeOutcome {
    /// The chosen opponent placed the Tribute +1/+1 counters (CR 702.104a).
    Paid,
    /// The chosen opponent declined (CR 702.104b: "if tribute wasn't paid").
    Declined,
}

/// A typed choice stored on a permanent (e.g., "choose a color" → Color(Red)).
/// The variant discriminant serves as the category key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum ChosenAttribute {
    Color(ManaColor),
    CreatureType(String),
    BasicLandType(BasicLandType),
    CardType(CoreType),
    OddOrEven(Parity),
    CardName(String),
    /// Stores a chosen number (e.g., "choose a number" for Talion).
    Number(u8),
    /// Stores the chosen opponent/player ID (CR 800.4a).
    Player(PlayerId),
    /// Stores two chosen colors as a pair.
    TwoColors([ManaColor; 2]),
    /// CR 702.104a + CR 702.104b: Records whether the opponent chosen for the
    /// Tribute ETB replacement paid tribute or declined. Read by the companion
    /// `TriggerCondition::TributeNotPaid` evaluator.
    TributeOutcome(TributeOutcome),
    /// CR 608.2d: Records the typed keyword chosen from a `ChoiceType::Keyword`
    /// option list (Urborg / Walking Sponge "choose an ability the target has,
    /// remove it"). Read by `ContinuousModification::RemoveChosenKeyword` at
    /// Layer 6 evaluation to strip the chosen keyword from the recipient.
    Keyword(Keyword),
    /// CR 614.12c: Records the anchor-word label chosen as the permanent
    /// entered the battlefield (e.g. Frostcliff Siege "Jeskai" / "Temur",
    /// Khans of Tarkir Sieges "Khans" / "Dragons"). Read by
    /// `StaticCondition::ChosenLabelIs` and `TriggerCondition::ChosenLabelIs`
    /// to gate which of the two anchor-word-marked abilities (CR 607: linked
    /// abilities) functions while the permanent is on the battlefield. The
    /// label is stored case-canonicalised to match `ChoiceType::Labeled`'s
    /// capitalised option list.
    Label(String),
    /// CR 613.1f + CR 611.2c + CR 400.7: A remembered card object — the "last
    /// chosen card" for cards like Koh, the Face Stealer ("Koh has all activated
    /// and triggered abilities of the last chosen card"). Unlike every other
    /// variant, this is NOT a player-prompted choice category: it is written by
    /// `Effect::RememberCard` directly from the resolution chain's tracked set,
    /// not produced through `ChoiceType`/`ChoiceValue`/`from_choice` (the same
    /// engine-set precedent as `TributeOutcome`). It is consumed by
    /// `TargetFilter::ChosenCard` at Layer-6 grant evaluation, and — being stored
    /// in `chosen_attributes` — is cleared automatically when the source permanent
    /// changes zones (CR 400.7), which is exactly the lifetime Koh's grant needs.
    /// Replace-on-rechoose: `RememberCard` removes any prior `Card` before pushing.
    Card(ObjectId),
    /// CR 608.2d + CR 122.1: The counter kind chosen from a `ChoiceType::CounterKind`
    /// option list (The Caves of Androzani "choose a counter on it"). Read by
    /// `Effect::PutChosenCounter` ("put an additional counter of that kind on
    /// that permanent") to add one counter of the chosen kind. Replace-on-rechoose
    /// (the per-iteration bind clears the prior `Counter` — see `bind_named_choice`).
    Counter(CounterType),
    /// CR 607.2d: The chosen seat direction (left/right) for a directional
    /// "choose left or right" ability linked to a "the [last] chosen direction"
    /// static (Pramikon, Sky Rampart; Mystic Barrier; Teyo, Geometric
    /// Tactician). Stored on the source's `chosen_attributes` and read by
    /// `GameObject::chosen_direction()` during the CR 508.1c attacker gate.
    /// Replace-on-rechoose: the {Left,Right} hijack in `choose.rs` removes any
    /// prior `Direction` before pushing, so only the "last chosen" survives.
    Direction(SeatDirection),
}

impl ChosenAttribute {
    /// Which category of choice this represents.
    pub fn choice_type(&self) -> ChoiceType {
        match self {
            Self::Color(_) => ChoiceType::color(),
            Self::CreatureType(_) => ChoiceType::creature_type(),
            Self::BasicLandType(_) => ChoiceType::BasicLandType,
            Self::CardType(_) => ChoiceType::card_type(),
            Self::OddOrEven(_) => ChoiceType::OddOrEven,
            Self::CardName(_) => ChoiceType::CardName,
            Self::Number(_) => ChoiceType::NumberRange {
                min: 0,
                max: 20,
                distinctness: NumberDistinctness::Repeatable,
            },
            // Player covers both Player and Opponent choice types
            Self::Player(_) => ChoiceType::Player,
            Self::TwoColors(_) => ChoiceType::TwoColors,
            // CR 702.104: Tribute outcome uses a dedicated prompt type rather than
            // a NamedChoice (two fixed labels: Paid / Declined). Classify under the
            // Labeled category so external listings (e.g., AI candidate generation)
            // can recognise it as a Yes/No-shaped prompt.
            Self::TributeOutcome(_) => ChoiceType::Labeled {
                options: vec!["Paid".to_string(), "Declined".to_string()],
            },
            // CR 608.2d: A category template — the concrete option list is
            // attached to each emission site (Urborg lists FirstStrike +
            // Swampwalk; another card might list any pair). Mirrors the
            // `NumberRange { min: 0, max: 20 }` template idiom.
            Self::Keyword(_) => ChoiceType::Keyword {
                options: Vec::new(),
                count: 1,
            },
            // CR 614.12c: Anchor-word labels are a free-form labeled choice
            // (the per-card option list — e.g. ["Jeskai", "Temur"] — is
            // emitted at the choice site, mirroring the `Keyword` template
            // idiom). The stored single label is one of those options.
            Self::Label(label) => ChoiceType::Labeled {
                options: vec![label.clone()],
            },
            // Engine-set, never player-prompted (written by `Effect::RememberCard`
            // from a tracked set, not via `from_choice`). `choice_type()` has no
            // callers for this variant; the empty `Labeled` template mirrors the
            // `Keyword`/`TributeOutcome` placeholder idiom rather than inventing a
            // spurious object-choice category.
            Self::Card(_) => ChoiceType::Labeled {
                options: Vec::new(),
            },
            // CR 608.2d + CR 122.1: A category template — the concrete counter-kind
            // option list is enumerated at each resolution from the target object's
            // counters (mirrors the `Keyword` template idiom).
            Self::Counter(_) => ChoiceType::CounterKind {
                options: Vec::new(),
            },
            // CR 607.2d: The directional choice is a two-option labeled prompt.
            // The {Left,Right} hijack in `choose.rs` recognises exactly this
            // option set to persist a typed `Direction` rather than a `Label`.
            Self::Direction(_) => ChoiceType::Labeled {
                options: vec!["Left".into(), "Right".into()],
            },
        }
    }

    /// Parse a player's string response into a typed ChosenAttribute.
    /// Returns None if the string doesn't match the expected choice type.
    pub fn from_choice(choice_type: ChoiceType, value: &str) -> Option<Self> {
        match ChoiceValue::from_choice(&choice_type, value)? {
            ChoiceValue::Color(color) => Some(Self::Color(color)),
            ChoiceValue::CreatureType(creature_type) => Some(Self::CreatureType(creature_type)),
            ChoiceValue::BasicLandType(land_type) => Some(Self::BasicLandType(land_type)),
            ChoiceValue::CardType(card_type) => Some(Self::CardType(card_type)),
            ChoiceValue::OddOrEven(parity) => Some(Self::OddOrEven(parity)),
            ChoiceValue::CardName(card_name) => Some(Self::CardName(card_name)),
            ChoiceValue::Player(id) => Some(Self::Player(id)),
            ChoiceValue::TwoColors(colors) => Some(Self::TwoColors(colors)),
            ChoiceValue::Number(n) => Some(Self::Number(n)),
            ChoiceValue::Keyword(keyword) => Some(Self::Keyword(keyword)),
            // CR 614.12c: Persist a labeled choice as an anchor-word label so
            // companion `ChosenLabelIs` conditions (static + trigger) can read
            // it for the lifetime of the permanent.
            ChoiceValue::Label(label) => Some(Self::Label(label)),
            // CR 608.2d + CR 122.1: Persist the chosen counter kind so a later
            // `Effect::PutChosenCounter` can read it.
            ChoiceValue::Counter(counter_type) => Some(Self::Counter(counter_type)),
            ChoiceValue::CardPredicate(_) => None,
            ChoiceValue::LandType(_) => None,
        }
    }
}

/// A typed value chosen at resolution time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum ChoiceValue {
    Color(ManaColor),
    CreatureType(String),
    BasicLandType(BasicLandType),
    CardType(CoreType),
    OddOrEven(Parity),
    CardName(String),
    Number(u8),
    Label(String),
    CardPredicate(CardPredicateChoice),
    LandType(String),
    Player(PlayerId),
    TwoColors([ManaColor; 2]),
    /// CR 608.2d: typed-keyword choice from a `ChoiceType::Keyword` option
    /// list (Urborg / Walking Sponge). Persisted into the source's
    /// `chosen_attributes` as `ChosenAttribute::Keyword` for later
    /// `RemoveChosenKeyword` resolution.
    Keyword(Keyword),
    /// CR 608.2d + CR 122.1: typed counter-kind choice from a
    /// `ChoiceType::CounterKind` option list (The Caves of Androzani). Persisted
    /// as `ChosenAttribute::Counter` for later `PutChosenCounter` resolution.
    Counter(CounterType),
}

impl ChoiceValue {
    pub fn from_choice(choice_type: &ChoiceType, value: &str) -> Option<Self> {
        match choice_type {
            ChoiceType::Color { excluded } => {
                let color = value.parse::<ManaColor>().ok()?;
                (!excluded.contains(&color)).then_some(Self::Color(color))
            }
            ChoiceType::CreatureType { .. } => Some(Self::CreatureType(value.to_string())),
            ChoiceType::BasicLandType => {
                value.parse::<BasicLandType>().ok().map(Self::BasicLandType)
            }
            ChoiceType::CardType { excluded } => {
                let core_type = value.parse::<CoreType>().ok()?;
                (!excluded.contains(&core_type)).then_some(Self::CardType(core_type))
            }
            ChoiceType::OddOrEven => value.parse::<Parity>().ok().map(Self::OddOrEven),
            ChoiceType::CardName => Some(Self::CardName(value.to_string())),
            ChoiceType::NumberRange { .. } => value.parse::<u8>().ok().map(Self::Number),
            ChoiceType::Labeled { .. } => Some(Self::Label(value.to_string())),
            ChoiceType::CardPredicate { options } | ChoiceType::CardPredicateGuess { options } => {
                let predicate = CardPredicateChoice::from_label(value)?;
                options
                    .contains(&predicate)
                    .then_some(Self::CardPredicate(predicate))
            }
            ChoiceType::LandType => Some(Self::LandType(value.to_string())),
            // CR 800.4a: Parse player ID from string.
            ChoiceType::Opponent { .. } | ChoiceType::Player => value
                .parse::<u8>()
                .ok()
                .map(|id| Self::Player(PlayerId(id))),
            ChoiceType::TwoColors => {
                let (a, b) = value.split_once(", ")?;
                let c1 = a.parse::<ManaColor>().ok()?;
                let c2 = b.parse::<ManaColor>().ok()?;
                Some(Self::TwoColors([c1, c2]))
            }
            ChoiceType::Word | ChoiceType::Artist => Some(Self::Label(value.to_string())),
            // CR 608.2d: match the player's response against the typed option
            // list by display string. Comparison is case-insensitive so the
            // frontend can render canonical capitalization while the engine
            // accepts either form.
            ChoiceType::Keyword { options, .. } => {
                let needle = value.to_lowercase();
                options
                    .iter()
                    .find(|k| k.to_string().to_lowercase() == needle)
                    .cloned()
                    .map(Self::Keyword)
            }
            // CR 608.2d + CR 122.1: match the player's response against the typed
            // counter-kind option list by display string (case-insensitive, so
            // the frontend can render canonical capitalization).
            ChoiceType::CounterKind { options } => {
                let needle = value.to_lowercase();
                options
                    .iter()
                    .find(|k| k.as_str().to_lowercase() == needle)
                    .cloned()
                    .map(Self::Counter)
            }
        }
    }
}

/// How to specify a damage amount -- either a fixed integer or a variable reference.
/// Which category of chosen attribute to read as a subtype.
/// Used by `ContinuousModification::AddChosenSubtype` in layer evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChosenSubtypeKind {
    CreatureType,
    BasicLandType,
}

/// CR 105.3: set-versus-add axis for chosen-color continuous modifications —
/// the typed counterpart of the fixed-color `SetColor` / `AddColor` pair.
/// Parameterizes [`ContinuousModification::AddChosenColor`] so bare
/// "…is/are the chosen color" (set / replace) and "…in addition to their other
/// colors" (add / retain) share one primitive instead of a boolean flag or a
/// sibling-variant cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum ColorChangeMode {
    /// CR 105.3 default: the new color replaces all previous colors.
    #[default]
    Set,
    /// CR 105.3 "in addition": retain prior colors and gain the chosen color.
    Add,
}

/// Which players' zones to count across for zone-based quantity references.
///
/// CR 608.2 + CR 109.5: `Controller` and `ScopedPlayer` are distinct axes
/// during `player_scope` iteration. `Controller` always means the printed
/// ability's controller (the "you" axis from CR 109.5). `ScopedPlayer` means
/// the player whose iteration copy is currently running (e.g., each opponent
/// in "Each opponent mills half of *their* library, rounded up." — Maddening
/// Cacophony's kicker mode). Outside iteration, `ScopedPlayer` falls back
/// to `Controller`. Mirrors the `ControllerRef::You` vs
/// `ControllerRef::ScopedPlayer` split.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CountScope {
    /// CR 109.5: The printed ability's controller — "you" / "your" semantics.
    Controller,
    /// CR 108.3 + CR 404.2: The printed ability controller as owner for
    /// player-scoped queries over non-battlefield zones.
    Owner,
    /// CR 608.2 + CR 109.5: The currently iterated player during a
    /// `player_scope` resolution — "they" / "their" semantics relative to the
    /// iteration. Issue #310: distinguishes "their library" (per-iteration)
    /// from "your library" (always caster). Falls back to `Controller`
    /// outside iteration.
    ScopedPlayer,
    /// CR 607.2d + CR 608.2c: The source's linked persisted chosen player —
    /// "the chosen player's <zone>" on cards whose source stored
    /// `ChosenAttribute::Player`.
    SourceChosenPlayer,
    All,
    Opponents,
}

fn default_count_scope_controller() -> CountScope {
    CountScope::Controller
}

/// Which zone to count cards in (for `QuantityRef::ZoneCardCount`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ZoneRef {
    Graveyard,
    Exile,
    Library,
    Hand,
}

/// CR 701.10d-f: What aspect to double (counters, life total, or mana pool).
/// Used by `Effect::Double` per locked decision D-05.
/// DoublePT/DoublePTAll handle CR 701.10a-c (power/toughness) separately.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum DoubleTarget {
    /// CR 701.10e: Double the number of a kind of counter on a permanent.
    /// None = all counter types on the permanent.
    Counters { counter_type: Option<CounterType> },
    /// CR 701.10d: Double a player's life total.
    LifeTotal,
    /// CR 701.10f: Double the amount of a type of mana in a player's mana pool.
    /// None = all mana colors.
    ManaPool { color: Option<ManaColor> },
}

/// CR 701.10a: Which P/T characteristics to double.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DoublePTMode {
    Power,
    Toughness,
    PowerAndToughness,
}

/// CR 122.5 / CR 122.8: Whether a counter-transfer effect actually moves
/// counters off the source or only puts matching counters on the destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CounterTransferMode {
    /// CR 122.5: Remove counters from the source and put them on the target.
    Move,
    /// CR 122.8: Put matching counters on the target using source/LKI state.
    Put,
}

/// CR 122.1 (Clockspinning): which counter operations the controller may choose
/// among, at resolution, for a player-chosen counter kind already present on the
/// target. A typed axis (not two `bool`s) so the operation set is
/// self-documenting and exhaustively matchable, mirroring `CounterTransferMode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CounterAdjustment {
    /// "put another of those counters on it" — add one of the chosen kind.
    Add,
    /// "remove that counter" — remove one of the chosen kind.
    Remove,
    /// Clockspinning: "remove that counter ... or put another of those counters
    /// on it" — the controller may add OR remove one of the chosen kind.
    AddOrRemove,
}

impl CounterAdjustment {
    /// Whether an "add one of the chosen kind" branch should be offered.
    pub fn allows_add(self) -> bool {
        matches!(self, Self::Add | Self::AddOrRemove)
    }

    /// Whether a "remove one of the chosen kind" branch should be offered.
    pub fn allows_remove(self) -> bool {
        matches!(self, Self::Remove | Self::AddOrRemove)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CounterMoveSelection {
    #[default]
    StackTarget,
    StackTargetAnyNumber,
    ResolutionDistributionAnyNumber,
}

/// CR 701.6 + CR 608.2c: A follow-up instruction carried by `Effect::Counter`
/// that acts on the *source permanent* of an ability countered by the effect.
///
/// The rider only fires when the countered object was an activated or triggered
/// ability — per CR 701.8a / CR 110.1 a spell is not a permanent, so when a
/// spell is countered there is no permanent for the rider to act on and it is
/// skipped (the conditional "if a permanent's ability is countered this way" is
/// encoded structurally by this spell-vs-ability gate, not by a separate
/// `AbilityCondition`).
///
/// Both variants share one categorical axis — "an effect applied to the
/// countered ability's source permanent" — so they live on a single enum rather
/// than as sibling `Option` fields on `Effect::Counter` (parameterize, don't
/// proliferate).
/// serde default for `CounterSourceRider::LosesAbilities::duration` — keeps
/// card-data serialized before the field was added deserializing correctly
/// (CR 611.2a: the loses-abilities effect lasts until the counter source leaves
/// the battlefield).
fn duration_until_host_leaves_play() -> Box<Duration> {
    Box::new(Duration::UntilHostLeavesPlay)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CounterSourceRider {
    /// CR 611.2: "that permanent loses all abilities ..." — applies a static
    /// definition to the countered ability's source permanent
    /// (Tishana's Tidebinder).
    ///
    /// CR 611.2a: `duration` is the continuous effect's lifetime — Tishana's
    /// "for as long as this creature remains on the battlefield" is
    /// `Duration::UntilHostLeavesPlay`. Surfaced as an explicit field (rather
    /// than hard-coded in the resolver) so the "as long as" clause is visible
    /// in the serialized AST and threaded to the transient continuous effect.
    LosesAbilities {
        static_def: Box<StaticDefinition>,
        // Boxed: `Duration` is a large enum, and this rider is embedded in the
        // frequently-constructed `ZoneCounterImperativeAst`/`Effect::Counter`
        // holders — boxing keeps `CounterSourceRider` small (clippy
        // large_enum_variant).
        #[serde(default = "duration_until_host_leaves_play")]
        duration: Box<Duration>,
    },
    /// CR 701.8: "destroy that permanent" — destroys the countered ability's
    /// source permanent (Teferi's Response, Green Slime).
    Destroy,
}

/// CR 614.1a + CR 608.2n: the destination a spell is redirected to when an
/// "instead" replacement intercepts its move from the stack to a graveyard.
/// Two producers share this typed destination:
///   - the countered-spell redirect (Memory Lapse, Remand, Spell Crumple) —
///     `Effect::Counter.countered_spell_zone`;
///   - the cast-this-way rider (Torrential Gearhulk → `Exile`; Kylox's
///     Voltstrider → `Library { Bottom }`) — installed as a per-object
///     replacement when a `CastFromZone` grant finalizes.
///
/// CR 608.2n is the default move this replaces (a resolving instant/sorcery is
/// put into its owner's graveyard); CR 701.6a is the corresponding default for
/// a countered spell. CR 614.1a makes the "instead" clause a replacement that
/// redirects that move to the named destination.
///
/// `Exile` is a member of the type because the cast-this-way rider needs it
/// (Torrential Gearhulk). The COUNTER parser must NOT emit `Exile` into
/// `countered_spell_zone`, however: exile-on-counter is already handled by the
/// separate graveyard-exile sub-ability rider in `game::effects::counter`
/// (Force of Negation, No More Lies), so emitting it here too would
/// double-exile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SpellStackToGraveyardReplacement {
    /// "exile it instead" (Torrential Gearhulk cast-this-way rider). Excluded
    /// from the counter parser — see the type doc.
    Exile,
    /// "put it on top/bottom of its owner's library" (Memory Lapse,
    /// Spell Crumple; Kylox's Voltstrider → bottom).
    Library { position: LibraryPosition },
    /// "return it to its owner's hand" / "put it into its owner's hand"
    /// (Remand).
    Hand,
}

/// Power/toughness value -- either a fixed integer or a variable reference (e.g. "*", "X").
///
/// Custom Deserialize: accepts both the tagged format `{"type":"Fixed","value":2}` (new)
/// and plain strings like `"2"` or `"*"` (legacy card-data.json).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", content = "value")]
pub enum PtValue {
    Fixed(i32),
    Variable(String),
    Quantity(QuantityExpr),
}

impl<'de> serde::Deserialize<'de> for PtValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        match &value {
            serde_json::Value::String(s) => {
                // Legacy format: plain string like "2", "*", "1+*"
                match s.parse::<i32>() {
                    Ok(n) => Ok(PtValue::Fixed(n)),
                    Err(_) => Ok(PtValue::Variable(s.clone())),
                }
            }
            serde_json::Value::Number(n) => Ok(PtValue::Fixed(n.as_i64().unwrap_or(0) as i32)),
            serde_json::Value::Object(_) => {
                // New tagged format: {"type":"Fixed","value":2}
                #[derive(serde::Deserialize)]
                #[serde(tag = "type")]
                enum PtValueHelper {
                    Fixed { value: i32 },
                    Variable { value: String },
                    Quantity { value: QuantityExpr },
                }
                let helper: PtValueHelper =
                    serde_json::from_value(value).map_err(serde::de::Error::custom)?;
                match helper {
                    PtValueHelper::Fixed { value: n } => Ok(PtValue::Fixed(n)),
                    PtValueHelper::Variable { value: s } => Ok(PtValue::Variable(s)),
                    PtValueHelper::Quantity { value: q } => Ok(PtValue::Quantity(q)),
                }
            }
            _ => Err(serde::de::Error::custom(
                "expected string, number, or object for PtValue",
            )),
        }
    }
}

/// CR 605.1a + CR 107.4a: Whether a mana-production effect is the *base* mana
/// addition (e.g. a basic Forest tapping for `{G}`) or an *additional* mana
/// addition that piggy-backs on another mana event (e.g. Fertile Ground's
/// "adds an additional one mana"). Typed enum — never a bool — so the parser
/// and resolver can dispatch on the contribution role without conflating it
/// with other dimensions of mana production.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum ManaContribution {
    /// The mana addition stands on its own (most mana abilities).
    #[default]
    Base,
    /// The mana addition is additive — it augments another mana event
    /// (Utopia Sprawl, Fertile Ground, Wild Growth, Carpet of Flowers).
    Additional,
}

fn default_mana_contribution() -> ManaContribution {
    ManaContribution::Base
}

fn is_default_mana_contribution(c: &ManaContribution) -> bool {
    matches!(c, ManaContribution::Base)
}

/// CR 700.5: Which color set a devotion quantity counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", content = "value")]
pub enum DevotionColors {
    /// Count devotion to one or more fixed colors.
    Fixed(Vec<ManaColor>),
    /// Count devotion to the color chosen by the source's current choice effect
    /// or persisted chosen-color attribute.
    ChosenColor,
}

impl<'de> serde::Deserialize<'de> for DevotionColors {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        match &value {
            serde_json::Value::Array(_) => {
                let colors: Vec<ManaColor> =
                    serde_json::from_value(value).map_err(serde::de::Error::custom)?;
                Ok(DevotionColors::Fixed(colors))
            }
            serde_json::Value::Object(_) => {
                #[derive(serde::Deserialize)]
                #[serde(tag = "type", content = "value")]
                enum DevotionColorsHelper {
                    Fixed(Vec<ManaColor>),
                    ChosenColor,
                }
                let helper: DevotionColorsHelper =
                    serde_json::from_value(value).map_err(serde::de::Error::custom)?;
                match helper {
                    DevotionColorsHelper::Fixed(colors) => Ok(DevotionColors::Fixed(colors)),
                    DevotionColorsHelper::ChosenColor => Ok(DevotionColors::ChosenColor),
                }
            }
            _ => Err(serde::de::Error::custom(
                "expected array or object for DevotionColors",
            )),
        }
    }
}

/// Mana production descriptor for `Effect::Mana`.
///
/// Custom Deserialize: accepts both the tagged format `{"type":"Fixed","colors":["White"]}` (new)
/// and a plain array of `ManaColor` like `["White","Green"]` (legacy, pre-ManaProduction refactor).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type")]
pub enum ManaProduction {
    /// Produce one or more specific colors.
    Fixed {
        #[serde(default)]
        colors: Vec<ManaColor>,
        /// CR 605.1a: Whether this is base or additional (e.g. Fertile Ground) mana.
        #[serde(
            default = "default_mana_contribution",
            skip_serializing_if = "is_default_mana_contribution"
        )]
        contribution: ManaContribution,
    },
    /// Produce strictly colorless mana (e.g. "Add {C}").
    Colorless {
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
    },
    /// Produce a mixed bundle of fixed colorless + colored mana (e.g. "Add {C}{R}").
    Mixed {
        colorless_count: u32,
        colors: Vec<ManaColor>,
    },
    /// Produce N mana of one chosen color from the provided set.
    AnyOneColor {
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
        #[serde(default = "default_all_mana_colors")]
        color_options: Vec<ManaColor>,
        /// CR 605.1a: Whether this is base or additional (e.g. Fertile Ground) mana.
        #[serde(
            default = "default_mana_contribution",
            skip_serializing_if = "is_default_mana_contribution"
        )]
        contribution: ManaContribution,
    },
    /// Produce N mana where each unit can be chosen independently from the provided set.
    AnyCombination {
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
        #[serde(default = "default_all_mana_colors")]
        color_options: Vec<ManaColor>,
    },
    /// Produce N mana of a previously chosen color.
    ChosenColor {
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
        /// CR 605.1a: Whether this is base or additional (e.g. Utopia Sprawl) mana.
        #[serde(
            default = "default_mana_contribution",
            skip_serializing_if = "is_default_mana_contribution"
        )]
        contribution: ManaContribution,
        /// CR 106.1: Optional fixed color the player may produce *instead of* the
        /// chosen color (Cycle of Gates: "Add {G} or one mana of the chosen
        /// color"). `None` = pure chosen-color production (Utopia Sprawl class).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fixed_alternative: Option<ManaColor>,
    },
    /// CR 106.7: Produce mana of any color that a land an opponent controls could produce.
    /// Colors are computed dynamically at resolution time by inspecting opponent lands.
    OpponentLandColors {
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
    },
    /// CR 106.1 + CR 106.5 + CR 202.2c: Produce `count` mana, each freely chosen
    /// among the colors of an object identified by `scope` (Omnath, Locus of All:
    /// "add three mana in any combination of its colors"). Unlike the static
    /// `AnyCombination` (whose `color_options` are fixed at parse time), the color
    /// set here is computed dynamically at resolution time from the scoped object's
    /// colors (CR 202.2c). Its analog is the dynamic-color family
    /// (`AnyOneColorAmongPermanents` / `DistinctColorsAmongPermanents`), not the
    /// static `AnyCombination`. If the object has no colors, CR 106.5 applies and
    /// no mana is produced.
    AnyCombinationOfObjectColors {
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
        scope: ObjectScope,
    },
    /// CR 106.7 + CR 106.1b: Produce N mana of any **type** (W/U/B/R/G/C) that a
    /// land matching `land_filter` could produce. Differs from
    /// `OpponentLandColors` in that the choice axis is the full type set
    /// including colorless (Reflecting Pool, Naga Vitalist, Incubation Druid,
    /// Cactus Preserve, Horizon of Progress). The `land_filter` controls which
    /// lands contribute (typically `ControllerRef::You`, but parameterized so
    /// future opponent-/player-scoped printings slot in without a new variant).
    /// Per CR 106.7 the union ignores cost-payability of the surveyed lands'
    /// mana abilities — only the resulting *type set* matters. CR 106.5 applies
    /// when the union is empty (e.g. no matching lands → no mana).
    AnyTypeProduceableBy {
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
        land_filter: TargetFilter,
    },
    /// CR 605.1a + CR 406.1: Produce one mana of any of the colors among the cards
    /// linked to `source` via `state.exile_links` (e.g. Pit of Offerings —
    /// "Add one mana of any of the exiled cards' colors"). Colors are computed
    /// dynamically at resolution time; if no linked card is colored the ability
    /// produces no mana (CR 106.5).
    ChoiceAmongExiledColors {
        #[serde(default)]
        source: LinkedExileScope,
    },
    /// CR 605.3b + CR 106.1a: Produce one of several fixed multi-mana
    /// combinations chosen by the controller. Each option is a complete
    /// pre-specified sequence of mana types (e.g. Shadowmoor/Eventide filter
    /// lands: `Add {W}{W}, {W}{U}, or {U}{U}` yields options
    /// `[[W,W], [W,U], [U,U]]`). Unlike `AnyOneColor` (pick one color, repeat
    /// N times) the choice axis here is which complete combination to produce.
    ChoiceAmongCombinations {
        #[serde(default)]
        options: Vec<Vec<ManaColor>>,
    },
    /// CR 903.4 + CR 903.4f + CR 106.5: Produce N mana of one color chosen
    /// from the controller's commander(s)' combined color identity. Colors
    /// are computed dynamically at resolution time via
    /// `commander_color_identity`. If the color identity is empty (no
    /// commander, or a colorless commander), CR 106.5 applies and the
    /// ability produces no mana. Used by Path of Ancestry and Study Hall.
    AnyInCommandersColorIdentity {
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
        /// CR 605.1a: Whether this is base or additional mana.
        #[serde(
            default = "default_mana_contribution",
            skip_serializing_if = "is_default_mana_contribution"
        )]
        contribution: ManaContribution,
    },
    /// CR 106.1 + CR 109.1: Produce one mana of each distinct color among
    /// permanents matching a filter. "Gold", "multicolor", and "colorless" are
    /// not colors (CR 105.1), so each of W/U/B/R/G contributes at most once.
    /// Used by Faeburrow Elder's "{T}: For each color among permanents you
    /// control, add one mana of that color." Mirrors the structure of
    /// `QuantityRef::DistinctColorsAmongPermanents`.
    DistinctColorsAmongPermanents { filter: TargetFilter },
    /// CR 106.1 + CR 109.1: Produce N mana of one chosen color from the distinct
    /// colors present among permanents matching `filter`. Mox Amber class:
    /// "{T}: Add one mana of any color among legendary creatures and
    /// planeswalkers you control." Colors resolve dynamically at activation
    /// time; CR 106.5 applies when no matching permanent is colored.
    AnyOneColorAmongPermanents {
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
        filter: TargetFilter,
        #[serde(
            default = "default_mana_contribution",
            skip_serializing_if = "is_default_mana_contribution"
        )]
        contribution: ManaContribution,
    },
    /// CR 603.7c + CR 106.3: Produce one mana of the same type as the mana
    /// produced by the triggering `ManaAdded` event. Used by `TapsForMana`
    /// triggers of the form "add one mana of any type that land produced"
    /// (Vorinclex, Voice of Hunger; Dictate of Karametra). Resolves from
    /// `state.current_trigger_event` at resolution time; emits no mana if the
    /// current trigger event is absent or not a `ManaAdded` event (CR 106.5).
    TriggerEventManaType,
}

/// CR 607.2a + CR 406.6 + CR 610.3: Which exile-link relation a mana ability reads
/// when computing legal colors. Typed enum — never a bool — so future extensions
/// (e.g. links to a different host than `~` itself) slot in cleanly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum LinkedExileScope {
    /// Read `state.exile_links` keyed to this ability's source object
    /// (the activating permanent).
    #[default]
    ThisObject,
}

impl<'de> serde::Deserialize<'de> for ManaProduction {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        match &value {
            serde_json::Value::Array(_) => {
                // Legacy format: plain Vec<ManaColor> like ["White", "Green"]
                let colors: Vec<ManaColor> =
                    serde_json::from_value(value).map_err(serde::de::Error::custom)?;
                Ok(ManaProduction::Fixed {
                    colors,
                    contribution: ManaContribution::default(),
                })
            }
            serde_json::Value::Object(_) => {
                // New tagged format: {"type": "Fixed", "colors": [...]}
                #[derive(serde::Deserialize)]
                #[serde(tag = "type")]
                enum ManaProductionHelper {
                    Fixed {
                        #[serde(default)]
                        colors: Vec<ManaColor>,
                        #[serde(default = "default_mana_contribution")]
                        contribution: ManaContribution,
                    },
                    Colorless {
                        #[serde(default = "default_quantity_one")]
                        count: QuantityExpr,
                    },
                    AnyOneColor {
                        #[serde(default = "default_quantity_one")]
                        count: QuantityExpr,
                        #[serde(default = "default_all_mana_colors")]
                        color_options: Vec<ManaColor>,
                        #[serde(default = "default_mana_contribution")]
                        contribution: ManaContribution,
                    },
                    AnyCombination {
                        #[serde(default = "default_quantity_one")]
                        count: QuantityExpr,
                        #[serde(default = "default_all_mana_colors")]
                        color_options: Vec<ManaColor>,
                    },
                    ChosenColor {
                        #[serde(default = "default_quantity_one")]
                        count: QuantityExpr,
                        #[serde(default = "default_mana_contribution")]
                        contribution: ManaContribution,
                        #[serde(default)]
                        fixed_alternative: Option<ManaColor>,
                    },
                    OpponentLandColors {
                        #[serde(default = "default_quantity_one")]
                        count: QuantityExpr,
                    },
                    AnyCombinationOfObjectColors {
                        #[serde(default = "default_quantity_one")]
                        count: QuantityExpr,
                        scope: ObjectScope,
                    },
                    AnyTypeProduceableBy {
                        #[serde(default = "default_quantity_one")]
                        count: QuantityExpr,
                        land_filter: TargetFilter,
                    },
                    ChoiceAmongExiledColors {
                        #[serde(default)]
                        source: LinkedExileScope,
                    },
                    ChoiceAmongCombinations {
                        #[serde(default)]
                        options: Vec<Vec<ManaColor>>,
                    },
                    Mixed {
                        colorless_count: u32,
                        colors: Vec<ManaColor>,
                    },
                    AnyInCommandersColorIdentity {
                        #[serde(default = "default_quantity_one")]
                        count: QuantityExpr,
                        #[serde(default = "default_mana_contribution")]
                        contribution: ManaContribution,
                    },
                    DistinctColorsAmongPermanents {
                        filter: TargetFilter,
                    },
                    AnyOneColorAmongPermanents {
                        #[serde(default = "default_quantity_one")]
                        count: QuantityExpr,
                        filter: TargetFilter,
                        #[serde(default = "default_mana_contribution")]
                        contribution: ManaContribution,
                    },
                    TriggerEventManaType,
                }
                let helper: ManaProductionHelper =
                    serde_json::from_value(value).map_err(serde::de::Error::custom)?;
                Ok(match helper {
                    ManaProductionHelper::Fixed {
                        colors,
                        contribution,
                    } => ManaProduction::Fixed {
                        colors,
                        contribution,
                    },
                    ManaProductionHelper::Colorless { count } => {
                        ManaProduction::Colorless { count }
                    }
                    ManaProductionHelper::AnyOneColor {
                        count,
                        color_options,
                        contribution,
                    } => ManaProduction::AnyOneColor {
                        count,
                        color_options,
                        contribution,
                    },
                    ManaProductionHelper::AnyCombination {
                        count,
                        color_options,
                    } => ManaProduction::AnyCombination {
                        count,
                        color_options,
                    },
                    ManaProductionHelper::ChosenColor {
                        count,
                        contribution,
                        fixed_alternative,
                    } => ManaProduction::ChosenColor {
                        count,
                        contribution,
                        fixed_alternative,
                    },
                    ManaProductionHelper::OpponentLandColors { count } => {
                        ManaProduction::OpponentLandColors { count }
                    }
                    ManaProductionHelper::AnyCombinationOfObjectColors { count, scope } => {
                        ManaProduction::AnyCombinationOfObjectColors { count, scope }
                    }
                    ManaProductionHelper::AnyTypeProduceableBy { count, land_filter } => {
                        ManaProduction::AnyTypeProduceableBy { count, land_filter }
                    }
                    ManaProductionHelper::ChoiceAmongExiledColors { source } => {
                        ManaProduction::ChoiceAmongExiledColors { source }
                    }
                    ManaProductionHelper::ChoiceAmongCombinations { options } => {
                        ManaProduction::ChoiceAmongCombinations { options }
                    }
                    ManaProductionHelper::Mixed {
                        colorless_count,
                        colors,
                    } => ManaProduction::Mixed {
                        colorless_count,
                        colors,
                    },
                    ManaProductionHelper::AnyInCommandersColorIdentity {
                        count,
                        contribution,
                    } => ManaProduction::AnyInCommandersColorIdentity {
                        count,
                        contribution,
                    },
                    ManaProductionHelper::DistinctColorsAmongPermanents { filter } => {
                        ManaProduction::DistinctColorsAmongPermanents { filter }
                    }
                    ManaProductionHelper::AnyOneColorAmongPermanents {
                        count,
                        filter,
                        contribution,
                    } => ManaProduction::AnyOneColorAmongPermanents {
                        count,
                        filter,
                        contribution,
                    },
                    ManaProductionHelper::TriggerEventManaType => {
                        ManaProduction::TriggerEventManaType
                    }
                })
            }
            _ => Err(serde::de::Error::custom(
                "expected array or object for ManaProduction",
            )),
        }
    }
}

/// Parse-time template for mana spend restrictions.
///
/// Unlike [`ManaRestriction`](super::mana::ManaRestriction) which carries concrete values
/// on a `ManaUnit`, this enum is stored on `Effect::Mana` and resolved at production time
/// by reading runtime state (e.g., chosen creature type from the source object).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManaSpendRestriction {
    /// "Spend this mana only to cast spells."
    SpellOnly,
    /// "Spend this mana only to cast creature spells."
    SpellType(String),
    /// "Spend this mana only to cast a creature spell of the chosen type."
    /// Resolved at runtime from the source's `chosen_creature_type()`.
    ChosenCreatureType,
    /// CR 106.6: "Spend this mana only to cast creature spells or activate abilities of creatures."
    /// Combined restriction with OR semantics: allowed for spells of `spell_type` OR ability
    /// activations described by `ability` — `OfSpellType` restricts to abilities of permanents
    /// of `spell_type`, `Any` permits any ability ("… or to activate an ability").
    SpellTypeOrAbilityActivation {
        spell_type: String,
        ability: AbilityActivationScope,
    },
    /// "Spend this mana only to activate abilities."
    /// Cannot be used to cast spells; only for ability activation costs.
    ActivateOnly,
    /// CR 106.6: "Spend this mana only to activate power-up abilities." Keyed on
    /// the activating ability's keyword tag (Quinjet Technician).
    ActivateTagged(AbilityTag),
    /// "Spend this mana only on costs that include {X}."
    /// Only permits spending on spells or abilities with {X} in their cost.
    XCostOnly,
    /// "Spend this mana only to cast spells with flashback."
    SpellWithKeywordKind(KeywordKind),
    /// "Spend this mana only to cast spells with flashback from a graveyard."
    SpellWithKeywordKindFromZone { kind: KeywordKind, zone: Zone },
    /// CR 106.6: "Spend this mana only to cast spells with mana value N or
    /// greater" (or "or less"). Parameterized over [`Comparator`] so a single
    /// variant covers every mana-value threshold reading rather than
    /// proliferating per-threshold siblings ("parameterize, don't proliferate").
    /// `value` is the printed threshold N; `comparator` applies
    /// `spell_mana_value <cmp> value`.
    SpellWithManaValue { comparator: Comparator, value: u32 },
    /// CR 106.6 + CR 107.3 + CR 202.3: "Spend this mana only to cast [creature]
    /// spells with mana value N or greater **or** [creature] spells with {X} in
    /// their mana costs" (Helga, Skittish Seer; Troyan, Gutsy Explorer). Disjunction
    /// of cost criteria with optional spell-type narrowing — see
    /// [`ManaRestriction::OnlyForSpellMatchingCostCriteria`](super::mana::ManaRestriction::OnlyForSpellMatchingCostCriteria).
    SpellMatchingCostCriteria {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        spell_type: Option<String>,
        criteria: Vec<SpellCostCriterion>,
    },
    /// CR 105.2 + CR 106.6: "Spend this mana only to cast spells with exactly N
    /// colors" (also "N or more / N or fewer"; colorless = 0). Parameterized over
    /// [`Comparator`] — one variant per color-count reading. `count` is N.
    SpellWithColorCount { comparator: Comparator, count: u32 },
    /// CR 106.6 + CR 400.7: "Spend this mana only to cast spells from your
    /// graveyard" / "from exile" ([`From`](super::mana::ZoneSpendPolarity::From))
    /// and "from anywhere other than your hand"
    /// ([`NotFrom`](super::mana::ZoneSpendPolarity::NotFrom), Mm'menon, the Right
    /// Hand). Gates spending on the spell's cast-from zone alone — a distinct axis
    /// from [`ManaSpendRestriction::SpellWithKeywordKindFromZone`], which
    /// additionally requires a keyword. Resolved against `SpellMeta.cast_from_zone`.
    /// Carried as a [`ZoneSpend`] newtype payload whose custom `Deserialize`
    /// accepts the legacy bare-`Zone` serialized form for backward compatibility,
    /// mapping it to the inclusion reading.
    SpellFromZone(ZoneSpend),
    /// CR 106.6 + CR 116.2m + CR 709.5e: "Spend this mana only to unlock
    /// [a ]door[s]" — the special-action half of a spend restriction. A leaf of
    /// the [`ManaSpendRestriction::Any`] disjunction (Smoky Lounge: "cast Room
    /// spells and unlock doors"). Lowered to
    /// [`ManaRestriction::OnlyForSpecialAction(SpecialAction::UnlockDoor)`](super::mana::ManaRestriction::OnlyForSpecialAction),
    /// enforced when a Room's CR 709.5e unlock cost is paid through
    /// [`PaymentContext::SpecialAction`](super::mana::PaymentContext::SpecialAction).
    UnlockDoor,
    /// CR 106.6 + CR 708.4: "Spend this mana only to cast face-down spells"
    /// (Tin Street Gossip). Lowered to
    /// [`ManaRestriction::OnlyForFaceDownSpell`](super::mana::ManaRestriction::OnlyForFaceDownSpell),
    /// gated on `SpellMeta.is_face_down` at the spell-payment site.
    ///
    /// The runtime gate reads `SpellMeta.is_face_down`, sourced from the cast's
    /// face-down intent (`build_spell_meta`) rather than `obj.face_down`, so it
    /// correctly REJECTS exile-concealment casts (foretell/hideaway) whose
    /// `obj.face_down = true` but which are cast face up (CR 702.143c). This leaf
    /// is now production-live: CR 708.4 morph/megamorph/disguise face-down spell
    /// casting (`AlternativeCastKeyword::FaceDown` → `continue_cast_face_down`)
    /// routes the `{3}` face-down cost (CR 702.37c) through `PaymentContext::Spell`
    /// with `SpellMeta.is_face_down = true`, so the gate is satisfiable and a card
    /// whose spend restriction includes this leaf is absorbed at the `Effect::Mana`
    /// seam and coverage-supported via `ManaSpendRestriction::is_coverage_supported`.
    /// See
    /// [`ManaRestriction::OnlyForFaceDownSpell`](super::mana::ManaRestriction::OnlyForFaceDownSpell).
    FaceDownSpell,
    /// CR 106.6 + CR 116.2b + CR 702.37e: "Spend this mana only to turn
    /// permanents face up" / "turn creatures face up" — the morph/disguise
    /// turn-face-up special-action half of a spend restriction (Overgrown
    /// Zealot; Tin Street Gossip). A leaf of the [`ManaSpendRestriction::Any`]
    /// disjunction. Lowered to
    /// [`ManaRestriction::OnlyForSpecialAction(SpecialAction::TurnFaceUp)`](super::mana::ManaRestriction::OnlyForSpecialAction).
    /// The runtime gate is **live**: the `GameAction::TurnFaceUp` handler pays the
    /// morph/disguise/manifest turn-face-up cost through
    /// `PaymentContext::SpecialAction(TurnFaceUp)`, so such mana is spendable there
    /// and correctly rejected for any other context — see
    /// [`SpecialAction::TurnFaceUp`](super::mana::SpecialAction::TurnFaceUp).
    TurnPermanentFaceUp,
    /// CR 106.6: Disjunction of spend restrictions ("cast X or Y or activate Z").
    /// Lowered to `ManaRestriction::OnlyForAny`.
    Any(Vec<ManaSpendRestriction>),
}

impl ManaSpendRestriction {
    /// Returns `true` iff this restriction is fully coverage-supported: every
    /// leaf named by the lowered [`ManaRestriction`](super::mana::ManaRestriction)
    /// has production-live semantics today. `Any(subs)` is coverage-supported iff
    /// it is non-empty and every sub-branch is coverage-supported.
    ///
    /// A `ManaSpendRestriction` with any unsupported leaf is left *unabsorbed* at
    /// the parser seam (see `parser::oracle_effect::sequence`), so the surrounding
    /// `Effect::Mana` line keeps a residual `Effect::Unimplemented` — honest
    /// coverage **red** — rather than marking a card supported while one branch it
    /// names is not production-live. As of CR 708.4 face-down spell casting,
    /// every named leaf is production-live: `FaceDownSpell` was the last dead
    /// leaf, now satisfiable at a `PaymentContext::Spell` site, so the only
    /// coverage-red result left is a structurally-empty `Any([])` (no branch to
    /// support). An `Any([FaceDownSpell, TurnPermanentFaceUp])` (Tin Street
    /// Gossip) is therefore coverage-supported.
    ///
    /// Any `grants` paired with an unsupported restriction drop with it. This is
    /// intentional: no real card pairs a mana-spell grant with a restriction that
    /// has an unsupported leaf, so nothing functional is lost.
    ///
    /// The match is exhaustive with **no wildcard arm** on purpose: a future
    /// `ManaSpendRestriction` variant must fail to compile here, forcing an
    /// explicit coverage classification rather than silently defaulting to a
    /// false green or false red.
    pub fn is_coverage_supported(&self) -> bool {
        match self {
            // LIVE — production coverage exists via `SpellMeta.has_x_in_cost`.
            ManaSpendRestriction::XCostOnly => true,
            // CR 708.4: gate is `meta.is_face_down`, which `build_spell_meta` now
            // sets `true` at a `PaymentContext::Spell` site for a morph/megamorph/
            // disguise face-down cast (`AlternativeCastKeyword::FaceDown` →
            // `continue_cast_face_down`, CR 702.37c), so the leaf is production-live.
            ManaSpendRestriction::FaceDownSpell => true,
            // LIVE — production-live leaves with parser/runtime coverage.
            // CR 116.2b + CR 702.37e / CR 702.168d / CR 701.40b: lowered to
            // `OnlyForSpecialAction(SpecialAction::TurnFaceUp)`, now satisfiable —
            // the `GameAction::TurnFaceUp` handler pays the morph/disguise/manifest
            // cost through `PaymentContext::SpecialAction(TurnFaceUp)`.
            ManaSpendRestriction::TurnPermanentFaceUp
            | ManaSpendRestriction::SpellOnly
            | ManaSpendRestriction::SpellType(_)
            | ManaSpendRestriction::ChosenCreatureType
            | ManaSpendRestriction::SpellTypeOrAbilityActivation { .. }
            | ManaSpendRestriction::ActivateOnly
            | ManaSpendRestriction::ActivateTagged(_)
            | ManaSpendRestriction::SpellWithKeywordKind(_)
            | ManaSpendRestriction::SpellWithKeywordKindFromZone { .. }
            | ManaSpendRestriction::SpellWithManaValue { .. }
            | ManaSpendRestriction::SpellMatchingCostCriteria { .. }
            | ManaSpendRestriction::SpellWithColorCount { .. }
            | ManaSpendRestriction::SpellFromZone(_)
            | ManaSpendRestriction::UnlockDoor => true,
            // CR 106.6: coverage for a disjunction requires every named branch to
            // be production-live (`.all()`). Partial absorption would drop
            // unsupported branches from coverage accounting. With `FaceDownSpell`
            // now live, only a structurally-empty `Any([])` is coverage-red.
            ManaSpendRestriction::Any(subs) => {
                !subs.is_empty() && subs.iter().all(ManaSpendRestriction::is_coverage_supported)
            }
        }
    }
}

/// Duration for temporary effects.
///
/// Player-axis variants are parameterized by `PlayerScope` per the workspace
/// "Parameterize, don't proliferate" principle. `PlayerScope::Controller`
/// recovers the legacy "until your next turn" / "controller's next step"
/// semantics; future `Target` / `Opponent` / `AllPlayers` readings unblock
/// cards whose duration is bound to a non-controller player.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Duration {
    /// CR 514.2: Effect expires at end of turn (cleanup step).
    UntilEndOfTurn,
    /// CR 514.2: Effect expires at end of combat phase.
    UntilEndOfCombat,
    /// CR 514.2 + CR 611.2a: Effect expires at the beginning of `player`'s
    /// next turn. `PlayerScope::Controller` corresponds to the legacy
    /// "until your next turn" reading.
    UntilNextTurnOf {
        player: PlayerScope,
    },
    /// CR 514.2: Effect expires at the **cleanup step of `player`'s next turn**
    /// — it persists through that entire turn. This is the "until the end of
    /// [your/their] next turn" reading (Light Up the Stage, Slip Out the Back),
    /// distinct from `UntilNextTurnOf` which expires at the *beginning* of the
    /// next turn. At the player's next untap step the effect is "armed"
    /// (converted to `UntilEndOfTurn`) so the existing cleanup-step prune ends
    /// it at that turn's cleanup; it survives the creation turn's own cleanup.
    UntilEndOfNextTurnOf {
        player: PlayerScope,
    },
    /// CR 611.2a: Effect expires when the source object leaves the
    /// battlefield.
    UntilHostLeavesPlay,
    /// CR 500.1 + CR 611.2a: Effect expires at the beginning of `player`'s
    /// next named phase/step. `Phase::Untap` covers exert / "doesn't untap"
    /// effects (CR 502.3). `Phase::End` covers "until your next end step"
    /// floating play-permission patterns such as Rocco, Street Chef (CR 513.1).
    #[serde(alias = "UntilNextUntapStepOf")]
    UntilNextStepOf {
        #[serde(default)]
        step: Phase,
        player: PlayerScope,
    },
    /// CR 611.2b: "for as long as [condition]" — effect persists while condition holds.
    ForAsLongAs {
        condition: StaticCondition,
    },
    /// CR 611.2a + CR 607.2a: Permission/effect lasts until the same linked
    /// source exiles another card. Used by "you may play that card until you
    /// exile another card with [this object]" source-linked exile grants.
    UntilSourceExilesAnotherCard,
    Permanent,
}

// ---------------------------------------------------------------------------
// Game restriction system — composable runtime restrictions
// ---------------------------------------------------------------------------

/// A game-level restriction that modifies how rules are applied.
/// Stored in `GameState::restrictions` and evaluated by relevant game systems.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum GameRestriction {
    /// CR 614.16: Damage prevention effects are suppressed.
    DamagePreventionDisabled {
        source: ObjectId,
        expiry: RestrictionExpiry,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<RestrictionScope>,
    },
    /// CR 101.2 + CR 601.2a + CR 602.5: A temporary effect prohibits one activity
    /// axis for affected players until expiry.
    ProhibitActivity {
        source: ObjectId,
        affected_players: RestrictionPlayerScope,
        expiry: RestrictionExpiry,
        activity: ProhibitedActivity,
    },
    /// CR 611.2a + CR 614.1d: A resolution-generated continuous effect prohibiting
    /// objects matching `filter` from entering the battlefield until `expiry`.
    /// The floating, duration-bound form of `StaticMode::CantEnterBattlefieldFrom`
    /// (Grafdigger's Cage) — e.g. Bad Wolf Bay: "cards can't enter from exile this
    /// turn." `filter` encodes both the subject (type) and the origin zone via
    /// `FilterProp::InAnyZone`, matched against the object's current zone at the
    /// zones.rs entry gate (the object is still in its origin zone when the gate
    /// runs). `source` is informational (CR 611.2a: the effect is source-
    /// independent once created).
    CantEnterBattlefieldFrom {
        source: ObjectId,
        expiry: RestrictionExpiry,
        filter: TargetFilter,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ProhibitedActivity {
    /// CR 101.2 + CR 601.2a: Restrict casting to the listed zones.
    CastOnlyFromZones { allowed_zones: Vec<Zone> },
    /// CR 101.2: Prevent casting matching spells.
    CastSpells {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        spell_filter: Option<TargetFilter>,
    },
    /// CR 101.2 + CR 602.5 + CR 605.1a: Prevent activating abilities, optionally
    /// exempting mana abilities. `only_tag = None` prohibits all activations;
    /// `Some(tag)` scopes the prohibition to abilities carrying that keyword tag
    /// (Kang → power-up), leaving every other activation legal.
    ActivateAbilities {
        exemption: ActivationExemption,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        only_tag: Option<AbilityTag>,
    },
    /// CR 508.1c: A temporary effect prohibits an affected player from declaring
    /// attacks against the defended scope ("that player can't attack you [or your
    /// permanents/planeswalkers]"). The scope rides the shared `AttackTargetFilter`
    /// so the declare-attackers gate reuses the same defended-scope matcher as
    /// static `CantAttack` restrictions.
    Attack {
        defended: crate::types::triggers::AttackTargetFilter,
    },
    /// CR 116.2a + CR 305.1 + CR 601.2a: Prohibit *playing* (casting a spell OR
    /// playing a land) cards located in `zone` for the affected players.
    ///
    /// Distinct from `CastOnlyFromZones` — that is an ALLOW-list restricted to
    /// CASTS only. A land play is a special action, not the casting of a spell
    /// (CR 116.2a, CR 305.1), so an allow-list of cast-legal zones cannot express
    /// "can't play cards from your hand" (Memory Vessel: each player may play the
    /// cards they exiled, but not from their hand). This is a DENY axis covering
    /// both the cast gate and the play-land gate, so it is categorically a
    /// separate variant, not a parameterization of `CastOnlyFromZones`.
    ProhibitPlayFromZone { zone: Zone },
    /// CR 305.1 + CR 116.2a: Prevent playing lands matching `land_filter`.
    ///
    /// The land-play sibling of `CastSpells { spell_filter }` on the filter-
    /// scoped deny axis. Kept as a separate variant rather than a parameter on
    /// `CastSpells` because CR 601.2a (casting) and CR 305.1 (land-playing, a
    /// special action per CR 116.2a — explicitly NOT a cast) are different rule
    /// sections; the same reasoning `ProhibitPlayFromZone` above already gives
    /// for staying separate from `CastOnlyFromZones` applies here. `None` denies
    /// every land (a blanket temporary land-play ban); `Some(filter)` scopes the
    /// deny to matching lands only (Conjurer's Ban: `HasChosenName`).
    PlayLands {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        land_filter: Option<TargetFilter>,
    },
}

/// When a game restriction expires.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum RestrictionExpiry {
    EndOfTurn,
    EndOfCombat,
    UntilPlayerNextTurn {
        player: PlayerId,
    },
    /// CR 514.2 + CR 500.7: Mirrors `Duration::UntilEndOfNextTurnOf`. Created
    /// pre-armed; at `player`'s next untap step it is CONVERTED to
    /// `RestrictionExpiry::EndOfTurn` (mirroring `prune_until_next_turn_effects`),
    /// so the existing cleanup-step prune ends it at THAT turn's cleanup — it
    /// persists through `player`'s entire next turn (Kang's extra turn). It
    /// survives the creating turn's own cleanup because that turn's untap step
    /// already passed before creation.
    UntilEndOfNextTurnOf {
        player: PlayerId,
    },
}

/// Limits the scope of a game restriction to specific sources or targets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum RestrictionScope {
    SourcesControlledBy(PlayerId),
    SpecificSource(ObjectId),
    DamageToTarget(ObjectId),
}

/// Identifies which players are affected by a temporary game restriction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum RestrictionPlayerScope {
    AllPlayers,
    SpecificPlayer(PlayerId),
    /// Placeholder used by parser lowering for effects that target a player.
    /// Resolved to `SpecificPlayer` by `add_restriction` at resolution time.
    TargetedPlayer,
    /// CR 608.2c: Anaphoric "that player" in a sub-ability reuses a player
    /// target already chosen for an earlier instruction in the same chain.
    /// Resolved to `SpecificPlayer` by `add_restriction` after parent target
    /// propagation, without declaring a second target slot.
    ParentTargetedPlayer,
    /// CR 109.4 + CR 608.2c + CR 608.2h: The controller of the parent effect's
    /// object target — "its controller" where "it" is the object countered/
    /// destroyed/tapped by the parent instruction (Render Silent: "Counter target
    /// spell. Its controller can't cast spells this turn."). Resolved to
    /// `SpecificPlayer` by `add_restriction::fill_runtime_fields` at restriction-
    /// creation time via `ability_utils::parent_target_controller`, capturing the
    /// referent's controller from last-known information (the object has left the
    /// stack by the time the chained restriction resolves). Distinct from
    /// `ParentTargetedPlayer` ("that player", CR 608.2c — a reused *player* target):
    /// this derives a player from an *object* target's controller (CR 109.4).
    /// Mirrors `PlayerFilter::ParentObjectTargetController` /
    /// `PlayerScope::ParentObjectTargetController` on the sibling enums.
    ParentObjectTargetController,
    OpponentsOfSourceController,
    /// CR 508.5 / CR 508.5a: The defending player for the source's attack
    /// ("Whenever ~ attacks, defending player can't cast spells this turn." —
    /// Xantid Swarm). Resolved to `SpecificPlayer` by `add_restriction` at
    /// resolution time via `combat::defending_player_for_attacker`, capturing
    /// the player as the restriction is created (the defending player is fixed
    /// once attackers are declared and does not change for the turn).
    DefendingPlayer,
    /// CR 109.5 + CR 118.12: The player of the current `player_scope` iteration —
    /// "each opponent who does can't attack you … during their next turn" (The
    /// Second Doctor), "each player who does can't attack you …" (City Hall).
    /// Inside a per-player optional effect, the consequence restriction binds to
    /// the specific player who performed the optional action. Resolved to
    /// `SpecificPlayer(ability.scoped_player.unwrap_or(controller))` by
    /// `add_restriction` at resolution time — the same lower-at-creation contract
    /// as `TargetedPlayer`/`DefendingPlayer`. Mirrors the existing
    /// `ControllerRef::ScopedPlayer` / `TargetFilter::ScopedPlayer` siblings.
    ScopedPlayer,
}

// ---------------------------------------------------------------------------
// Casting permissions — per-object casting grants
// ---------------------------------------------------------------------------

/// A permission granted to a `GameObject` allowing it to be cast under specific conditions.
/// Stored in `GameObject::casting_permissions`.
// clippy::large_enum_variant: like `Effect`, this is a central engine enum whose
// variants legitimately carry large engine payloads directly (`ExileWithAltCost`
// holds a `ManaCost`, an `Option<ResolutionCastCleanup>`, an
// `Option<CastPermissionConstraint>`, and the typed graveyard-redirect
// destination). Boxing one incidental field would not meaningfully shrink the
// 500+ byte variant; the permissions are short-lived per-object grants, not
// hot-path bulk collections, so the size is intentional. Mirrors the documented
// allow on `Effect`.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CastingPermission {
    /// CR 715.5: After Adventure resolves to exile, creature face castable from exile.
    AdventureCreature,
    /// Card may be cast from exile for the specified cost by its owner.
    /// Building block for Airbending, Suspend, and similar "cast from exile" mechanics.
    /// `cast_transformed` causes the spell to resolve to its back face (CR 712.14a); used
    /// by Siege victory triggers (CR 310.11b: "cast it transformed without paying its mana cost").
    ExileWithAltCost {
        cost: ManaCost,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        cast_transformed: bool,
        /// CR 702.85a: optional cast-time predicate gating whether the cast may
        /// proceed. `None` for the common case (Airbending, Suspend, Discover,
        /// etc.); `Some(...)` only for mechanics that must reject after X is
        /// chosen. Evaluated at cast finalization, before the spell moves to
        /// the stack — see `casting_costs::finalize_cast_with_phyrexian_choices`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        constraint: Option<CastPermissionConstraint>,
        /// CR 611.2a + CR 118.9: When `Some(p)`, only player `p` may cast under
        /// this permission. The grant-issuing effect (typically an attack
        /// trigger or ETB on a different controller's permanent) records the
        /// controller of the ability that granted this permission so cards
        /// owned by an *opponent* (e.g., cards exiled from each player's
        /// library by Jeleva's ETB) can still be cast only by the granting
        /// permanent's controller — not by the card's owner. When `None`,
        /// `has_exile_cast_permission` falls back to the legacy
        /// `obj.owner == player` rule (Discover, Cascade, Suspend, Airbending,
        /// and other classes where the card is exiled from the would-be
        /// caster's own zones, so owner == grantee anyway).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        granted_to: Option<PlayerId>,
        /// CR 608.2g: When `Some(...)`, this permission was granted to cast a
        /// Cascade/Discover hit *during resolution* of the source spell. It
        /// carries the rejection-cleanup state (exiled misses + where the hit
        /// goes if the cast-time MV check fails). `None` for all standing
        /// permissions (Airbending, Maralen, Beseech, etc.) which are
        /// cast later via a normal `CastSpell` and never need resolution-time
        /// cleanup. `resolution_cleanup.is_some()` is the discriminator that
        /// distinguishes a cast-during-resolution permission from a plain
        /// `ManaValue`-constrained standing permission at finalize time.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resolution_cleanup: Option<ResolutionCastCleanup>,
        /// CR 611.2a: Optional durational scope. When `Some(...)`, this
        /// permission is pruned by the corresponding `layers::prune_*` helpers
        /// at the same timing points as `PlayFromExile { duration, .. }`.
        /// `None` (the common case) preserves the standing behavior: the
        /// permission persists until the object leaves exile (Airbending,
        /// Suspend, Discover, Cascade, etc., handled by
        /// `zones::apply_zone_exit_cleanup`).
        ///
        /// CR 702.88a (Rebound): used by the Rebound recast permission with
        /// `Duration::UntilEndOfTurn` so the granted "cast this card from
        /// exile without paying its mana cost" offer expires at the cleanup
        /// step of the upkeep on which it was offered if the controller
        /// declines or fails to cast.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration: Option<Duration>,
        /// CR 614.1a + CR 608.2n: Torrential Gearhulk / Kylox's Voltstrider
        /// class — a `CastFromZone` grant whose sub-ability is "if that spell
        /// would be put into a graveyard, [exile it / put it on the bottom of
        /// its owner's library / return it to its owner's hand] instead."
        /// `Some(dest)` installs the destination-correct CR 608.2n redirect
        /// replacement when the granted cast finalizes; `None` (the common
        /// case) leaves the resolving spell on its default graveyard path.
        /// Typed rather than a bool so the rider covers every CR 614.1a
        /// destination, not just exile. The `alias` + `deserialize_with` accept
        /// the legacy `exile_instead_of_graveyard_on_resolve: bool` this field
        /// replaced (`true` → `Some(Exile)`, `false`/absent → `None`), so an
        /// in-flight saved / reconnected permission serialized before the
        /// migration still reloads with its exile-on-resolution rider.
        #[serde(
            default,
            alias = "exile_instead_of_graveyard_on_resolve",
            skip_serializing_if = "Option::is_none",
            deserialize_with = "deserialize_graveyard_replacement_compat"
        )]
        graveyard_replacement: Option<SpellStackToGraveyardReplacement>,
        /// CR 614.1c + CR 122.1: Osteomancer Adept / The Tomb of Aclazotz class —
        /// a `CastFromZone` grant whose sub-ability is "the creature cast this way
        /// enters with a [counter] counter on it." When `Some(ct)`, the granted
        /// cast finalization registers a pending ETB counter of type `ct` on the
        /// cast object so it enters the battlefield carrying that counter
        /// (CR 122.1h: a finality counter intrinsically creates the replacement
        /// effect that exiles the permanent instead of letting it go to a
        /// graveyard). `None` for every other
        /// `CastFromZone` grant. Typed `Option<CounterType>` rather than a bool so
        /// the rider covers any counter the cast-this-way creature enters with.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        enters_with_counter: Option<CounterType>,
        /// CR 205.1b + CR 611.2c: The Tomb of Aclazotz class — a `CastFromZone`
        /// grant whose sub-ability is "the creature cast this way … is a
        /// [type] in addition to its other types." Continuous modifications
        /// (Layer 4, CR 613.1d) recorded on the grant so the cast finalization
        /// applies them as a `Duration::Permanent` `TransientContinuousEffect`
        /// on the cast object (CR 611.2c: the affected set is fixed to the one
        /// cast object). Empty for every grant without a type-grant rider.
        /// Parallel to `enters_with_counter` because the counter (CR 122) and
        /// the type grant (CR 613) are categorically distinct rule sections.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        enters_with_modifications: Vec<ContinuousModification>,
        /// CR 609.4b: Optional payment permission scoped to this specific grant.
        /// `AnyColor` and `AnyTypeOrColor` both relax colored mana requirements;
        /// only the latter also relaxes mana-type requirements. Read at payment
        /// time by `casting::player_can_spend_as_any_color_for_optional_spell`,
        /// keyed on `granted_to == player`. Mirrors
        /// `PlayFromExile.mana_spend_permission`; `None` (the common case) leaves
        /// payment unchanged for every other exile/graveyard alt-cost grant.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mana_spend_permission: Option<ManaSpendPermission>,
    },
    /// CR 400.7i: Play from exile until duration expires (impulse draw).
    /// Building block for "exile top N, choose one, you may play it this turn" patterns.
    ///
    /// `granted_to` records the player the permission was granted to — i.e. the
    /// controller of the effect that created it (CR 611.2a/b). This is required
    /// to correctly expire `Duration::UntilNextTurnOf` permissions at the
    /// grantee's next untap step and `Duration::UntilEndOfTurn` permissions at
    /// cleanup (CR 514.2). Cast-permission checks (`has_exile_cast_permission`)
    /// do not consult this field — it governs duration pruning only.
    PlayFromExile {
        duration: Duration,
        granted_to: PlayerId,
        /// CR 601.2a: Per-source use frequency for persistent play
        /// permissions. `Unlimited` preserves existing impulse-draw behavior;
        /// `OncePerTurn` models linked static permissions like Evelyn.
        #[serde(default, skip_serializing_if = "CastFrequency::is_unlimited")]
        frequency: CastFrequency,
        /// Source object whose once-per-turn slot is consumed when
        /// `frequency` is bounded, and whose later source-linked exile expires
        /// `Duration::UntilSourceExilesAnotherCard` grants. Filled by
        /// `grant_permission::resolve`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_id: Option<ObjectId>,
        /// Controller of the ability that exiled this card and attached this
        /// permission. Evelyn-class statics use this as provenance, then find
        /// a currently-live static permission source at play/cast time.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exiled_by_ability_controller: Option<PlayerId>,
        /// CR 609.4b: Optional payment permission carried by the same effect
        /// that allows the card to be played/cast from exile. This scopes
        /// "mana of any type can be spent to cast that spell" to the exiled
        /// card rather than creating a global player permission.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mana_spend_permission: Option<ManaSpendPermission>,
        /// CR 601.2a: Optional card-type/quality filter restricting WHICH of the
        /// granted-permission objects may actually be cast. `None` (the common
        /// case — Light Up the Stage, Reckless Impulse) authorizes any exiled
        /// card. `Some(filter)` scopes the grant to cards matching `filter`
        /// (Chandra, Hope's Beacon +1: "you may cast an instant or sorcery spell
        /// from among those exiled cards"). Enforced in
        /// `casting::play_from_exile_permission_source`, so the same gate covers
        /// both the cast path and the land-play path; a card that fails the
        /// filter is invisible to `has_exile_cast_permission`. Evaluated with a
        /// `FilterContext::neutral()` because card-type filters are printed
        /// object qualities, not source/controller-relative.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        card_filter: Option<TargetFilter>,
        /// CR 603.7 + CR 611.2a: Identity of the resolving tracked set for a
        /// `single_use` grant. This is deliberately separate from `source_id`:
        /// the same permanent can create overlapping "one spell from among
        /// those cards" effects, and each tracked set gets its own cast slot.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        single_use_group: Option<TrackedSetId>,
        /// CR 601.2a + CR 611.2a: When `true`, this grant authorizes at most ONE
        /// cast across its entire duration window — the "you may cast *a/one*
        /// [type] spell from among those exiled cards" class (Chandra, Hope's
        /// Beacon +1). Distinct from `frequency: OncePerTurn`, which resets each
        /// turn (CR 514.2): a single-use grant spanning two turns still permits
        /// only one cast total. On the finalizing cast, the shared grant is
        /// stripped from every exiled object carrying the same `single_use_group`
        /// (`casting::consume_single_use_play_from_exile`), making the remaining
        /// cards uncastable. `false` (default) preserves the unlimited
        /// within-window impulse-draw behavior.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        single_use: bool,
        /// CR 601.2f: Optional mana-cost raise for spells cast via this permission
        /// ("Each spell cast this way costs {1} more to cast." — Lightstall Inquisitor).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cast_cost_raise: Option<ManaCost>,
        /// CR 614.1c: Lands played via this permission enter with this tap state
        /// ("Each land played this way enters tapped." — Lightstall Inquisitor).
        /// "enters tapped" is a CR 614.1c "[permanent] enters ..." replacement.
        #[serde(default, skip_serializing_if = "EtbTapState::is_unspecified")]
        land_enter_tapped: EtbTapState,
        /// CR 611.2a: Additional non-time invalidation printed on the same
        /// play-from-exile permission. `None` preserves ordinary impulse grants;
        /// source-bound forms are pruned by the grant resolver when their
        /// printed invalidation event occurs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        invalidation: Option<PlayPermissionInvalidation>,
    },
    /// CR 122.3: Cast from exile by paying {E} equal to the card's mana value.
    /// Building block for Amped Raptor and similar energy-based casting mechanics.
    ExileWithEnergyCost,
    /// CR 118.9 + CR 119.4: Cast from exile by paying a non-mana alternative
    /// cost in lieu of the spell's mana cost. Building block for "play it ...
    /// pay [non-mana cost] rather than paying its mana cost" patterns
    /// (Nashi, Moon Sage's Scion: pay life equal to the spell's mana value).
    ///
    /// `cost` is a full `AbilityCost` (with dynamic-quantity support via
    /// `QuantityExpr`), distinguishing this from `ExileWithAltCost { cost:
    /// ManaCost }` which can only carry a fixed mana cost.
    ExileWithAltAbilityCost {
        /// CR 117.1: the alternative cost to pay instead of the spell's mana
        /// cost. Resolved through the standard `pay_additional_cost` pipeline,
        /// so `AbilityCost::PayLife { amount: QuantityExpr }` and friends
        /// (with dynamic quantity refs like `SelfManaValue`, which reads the
        /// spell-being-cast's mana value at cost-payment time) work for free.
        cost: AbilityCost,
        /// CR 702.85a: optional cast-time predicate gating whether the cast
        /// may proceed. Mirrors `ExileWithAltCost.constraint`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        constraint: Option<CastPermissionConstraint>,
        /// CR 611.2a + CR 118.9: Mirrors `ExileWithAltCost.granted_to`. See
        /// that field for the full rationale on owner-versus-grantee binding.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        granted_to: Option<PlayerId>,
    },
    /// CR 702.185a: Warp — card may be cast from exile at its normal mana cost,
    /// but only after the specified turn ends. Persists for as long as card remains exiled.
    WarpExile { castable_after_turn: u32 },
    /// CR 702.170a + CR 702.170d: Plot — card was exiled from its owner's hand via
    /// the plot special action (or granted this marker by another effect). On any
    /// turn after `turn_plotted`, the owner may cast it from exile without paying
    /// its mana cost during their own main phase while the stack is empty
    /// (sorcery-speed). The `turn_plotted` field is stamped from `state.turn_number`
    /// at resolution time by `grant_permission::resolve` (placeholder `0` at
    /// definition time); it is read by `has_exile_cast_permission` to gate the
    /// "later turn" check. Persists for as long as the card remains in exile
    /// (cleared by `zones::apply_zone_exit_cleanup` when the card leaves exile).
    Plotted { turn_plotted: u32 },
    /// CR 702.143a-d: Foretell — card was exiled from its owner's hand by the
    /// foretell special action, became a foretold card in exile, and may be
    /// cast on a later turn for its foretell cost. `turn_foretold` is stamped
    /// when the special action resolves; the permission is scoped to the exile
    /// zone and cleared when the object leaves exile.
    Foretold { cost: ManaCost, turn_foretold: u32 },
}

/// CR 611.2a: Non-time condition that invalidates a per-card play permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PlayPermissionInvalidation {
    /// "Until you exile another card with [this source]" — the next grant from
    /// the same source to the same player replaces earlier grants from that
    /// source without touching unrelated impulse permissions.
    UntilNextGrantFromSameSource,
}

/// CR 609.4b: Permission modifying how mana may be spent to pay a cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ManaSpendPermission {
    /// CR 609.4b + CR 106.1a: Mana may be spent as though it were mana of any
    /// color for this payment. This relaxes colored requirements but does not
    /// make colored mana satisfy a colorless (`{C}`) or snow (`{S}`) requirement.
    AnyColor,
    /// CR 609.4b + CR 106.1b: Mana may be spent as though it were mana of any
    /// type or color for this payment. This preserves the broader Oracle
    /// distinction without changing the actual mana spent.
    AnyTypeOrColor,
}

impl ManaSpendPermission {
    /// CR 609.4b: Projects the shared colored-requirement relaxation without
    /// claiming that `AnyColor` and `AnyTypeOrColor` are semantically equal.
    pub const fn allows_spending_as_any_color(self) -> bool {
        match self {
            Self::AnyColor | Self::AnyTypeOrColor => true,
        }
    }
}

/// CR 611.2a + CR 108.3: Identifies which player a `CastingPermission` is granted
/// to at resolution time. Resolved to a concrete `PlayerId` by
/// `grant_permission::resolve` before the permission is written to the target
/// `GameObject`. Drives both `granted_to` binding and, for durations scoped to
/// that player (e.g., `UntilNextTurnOf`), the prune step in `layers.rs`.
///
/// Default is `AbilityController` for all pre-existing parser call sites.
/// Additional variants support compound-exile patterns where multiple objects
/// are granted distinct permissions tied to different players:
/// - `ObjectOwner` — Suspend Aggression: "its owner may play it". Each object
///   in the tracked set is granted to its own owner (CR 108.3).
/// - `ParentTargetController` — Expedited Inheritance: "its controller may
///   exile [N] ... They may play those cards". The grant is tied to the
///   parent effect's player target rather than the triggered-ability controller.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PermissionGrantee {
    /// CR 611.2a default — the controller of the effect that created the grant.
    #[default]
    AbilityController,
    /// CR 108.3 — each iterated object's owner (per-object binding).
    ObjectOwner,
    /// CR 109.4 — the player target of the parent effect in the chain.
    ParentTargetController,
}

/// Returns true when `grantee` is the default (`AbilityController`). Used as a
/// `skip_serializing_if` predicate so pre-existing card JSON stays unchanged.
pub fn is_default_grantee(g: &PermissionGrantee) -> bool {
    matches!(g, PermissionGrantee::AbilityController)
}

/// CR 702.85a: Typed cast-time predicates attached to an `ExileWithAltCost`
/// permission. Extend this enum when future mechanics need cast-time gating
/// that cannot be evaluated at permission-grant time (e.g., X-cost spells
/// whose mana value is only known after the caster chooses X).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum CastPermissionConstraint {
    /// CR 202.3 + CR 601.2e: The spell's resulting mana value must satisfy
    /// this predicate for the cast permission to apply.
    ManaValue {
        comparator: Comparator,
        value: QuantityExpr,
    },
}

/// CR 608.2g: Rejection-cleanup state carried by a cast-during-resolution
/// `ExileWithAltCost` permission. Its presence is the engine's marker that the
/// cast happens *during the resolution* of its source ability (CR 608.2g —
/// normal timing/empty-stack/active-player gates do not apply). For Cascade /
/// Discover, when the cast-time resulting-mana-value check fails at
/// finalization the source spell's `WaitingFor::CastOffer` has already been
/// consumed, so the misses ride inside the permission and the engine still
/// knows where the rejected hit goes (`reject_action`). Suspend's last-counter
/// free cast (CR 702.62a/d) has no dig, so it carries an empty `exiled_misses`
/// and `RemainExiled` — the marker still arms the CR 608.2g timing bypass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolutionCastCleanup {
    /// CR 608.2g: The resolving Cascade/Discover/Ripple source that owns every
    /// post-offer library placement. The during-resolution cast can outlive the
    /// original `CastOffer`, so its cleanup payload is the typed source carrier.
    pub source_id: super::identifiers::ObjectId,
    /// Cards exiled/revealed during the dig that were not the hit.
    /// Empty for Suspend's self-free-cast (no dig).
    pub exiled_misses: Vec<super::identifiers::ObjectId>,
    /// Where the hit goes if the player declines or the cast-time MV check
    /// rejects the cast.
    pub reject_action: ResolutionMvRejectAction,
    /// What happens after the hit is successfully cast. Cascade/Discover bottom
    /// their misses immediately; Ripple may need to offer additional same-named
    /// cards from the same reveal first (CR 702.60a).
    #[serde(default)]
    pub success_action: ResolutionCastSuccessAction,
}

/// CR 608.2g + CR 609.4b: how a cast-during-resolution pays.
///
/// `Free` is the Cascade/Discover/Ripple/Suspend/free-window shape — the card is
/// cast without paying its mana cost, so the `ExileWithAltCost` permission zeros
/// the cost and payment is `Auto`. `FullCost` is the Quistis Trepe / Tinybones
/// the Pickpocket shape — "you may cast target X card from a graveyard, and mana
/// of any type can be spent to cast that spell." The caster pays the card's real
/// printed cost (CR 609.4b: any-type concession affects only *how* the cost is
/// paid, not the cost itself), so the permission carries `SelfManaCost` and
/// payment is `Manual` so a mana-payment window opens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ResolutionCastCost {
    /// Cast without paying the mana cost (Cascade, Discover, Ripple, Suspend,
    /// Invoke Calamity's free-cast window). Zero cost, `Auto` payment.
    Free,
    /// CR 609.4b: Cast paying the card's live printed cost. The optional
    /// `mana_spend_permission` rides onto the granted permission so mana of any
    /// type can be spent off-color (Quistis Trepe, Tinybones the Pickpocket).
    FullCost {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mana_spend_permission: Option<ManaSpendPermission>,
    },
    /// CR 118.9 + CR 702.62a: Cast paying a specific alternative mana cost
    /// borrowed from a keyword (e.g., The Face of Boe's suspend cost). The
    /// mana cost is explicit (not `SelfManaCost`) and payment is `Auto` so the
    /// pool is drained automatically during resolution (the triggering player's
    /// pool already holds the keyword cost before the trigger resolves). `None`
    /// from `effective_keyword_mana_cost` aborts before this is constructed
    /// (see `cast_from_zone::complete_hand_pick_cast_from_zone`).
    AlternativeMana { cost: crate::types::mana::ManaCost },
}

/// CR 608.2g: Disposition of a during-resolution card that is not cast.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ResolutionMvRejectAction {
    /// CR 702.85a: cascade — hit joins misses on the bottom in random order.
    BottomWithMisses,
    /// CR 701.57a: discover — hit goes to its owner's hand; misses to bottom.
    ToHand,
    /// CR 702.62a: Suspend's last-counter free cast — the card has no dig
    /// misses and no resulting-MV gate, so this reject disposition is only
    /// reached if a future during-resolution free cast adds a constraint. "If
    /// you don't [cast it], it remains exiled" — the card stays in exile.
    RemainExiled,
}

/// CR 608.2g: Follow-up after a during-resolution cast succeeds.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ResolutionCastSuccessAction {
    /// Cascade/Discover: cast hit is gone, so bottom the dig misses now.
    #[default]
    BottomMisses,
    /// CR 702.60a: Ripple can cast any number of same-named revealed cards. Keep
    /// offering the remaining hits before bottoming the non-hit revealed cards.
    RippleOfferRemaining {
        remaining_hits: Vec<super::identifiers::ObjectId>,
    },
    /// CR 608.2g + CR 601.2 + CR 202.3: Invoke Calamity's free-cast window — after
    /// a spell cast this way resolves, re-open the window with the cast count
    /// decremented and the running mana-value budget reduced by the spell's
    /// resulting mana value, until the count hits zero or the controller
    /// declines. Carries the window's parameters so the candidate set can be
    /// recomputed from the controller's current graveyard/hand.
    FreeCastOfferRemaining {
        controller: PlayerId,
        remaining_casts: u8,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remaining_mv_budget: Option<u32>,
        filter: TargetFilter,
        zones: Vec<Zone>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        exile_instead_of_graveyard: bool,
    },
}

/// CR 603.7b: lifetime of a one-shot `WhenNextEvent` delayed trigger. CR 603.7b
/// states a delayed triggered ability "will trigger only once—the next time its
/// trigger event occurs—unless it has a stated duration, such as 'this turn.'"
/// The firing semantics are identical (fire once on the next matching event);
/// the only difference is whether that stated "this turn" duration is present.
///
/// - `ThisTurn` (CR 603.7b "stated duration, such as 'this turn'"): "When you
///   next [event] **this turn** …" — bounded to the turn it was created. If the
///   event never occurs that turn, the unfired trigger is pruned at
///   end-of-turn cleanup. Coin-flip reflexives, "when you next cast a spell this
///   turn", "when you next attack this turn", etc.
/// - `Persistent` (CR 603.7b, no stated duration): an open-ended delayed trigger
///   that "will trigger only once—the next time its trigger event occurs" with
///   no "this turn" limit — it survives across turns until the event occurs. The
///   Pandorica's "When ~ becomes untapped or leaves the battlefield, that
///   permanent phases in" re-entry fires on a later turn's untap step, so it
///   must not be pruned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DelayedTriggerLifetime {
    /// CR 603.7b ("stated duration, such as 'this turn'"): bounded to the
    /// creating turn; pruned at cleanup if unfired.
    #[default]
    ThisTurn,
    /// CR 603.7b (no stated duration): open-ended; persists across turns until
    /// the event fires.
    Persistent,
    /// CR 603.12: a reflexive delayed trigger ("when you [do X] this way, …").
    /// It is checked immediately after creation and triggers only on the trigger
    /// event(s) that occurred earlier during the same resolution that created it.
    /// Unlike a plain `ThisTurn` `WhenNextEvent` (which would linger for a later
    /// same-turn matching event, CR 603.7b), a `Reflexive` gets exactly one shot
    /// on its creation resolution's event batch: if unmatched on that first
    /// `check_delayed_triggers` pass it is discarded rather than left pending.
    Reflexive,
}

impl DelayedTriggerLifetime {
    /// Serde skip-helper: `ThisTurn` is the default and is omitted from JSON.
    pub fn is_this_turn(&self) -> bool {
        matches!(self, DelayedTriggerLifetime::ThisTurn)
    }
}

/// CR 513.2 + CR 603.7a: turn-floor gate for an `AtNextPhaseForPlayer` delayed
/// trigger. The concrete floor only exists at delayed-trigger CREATION (CR
/// 603.7a — created on resolution), so the parser emits the symbolic
/// `AfterCreationTurn`; `effects::delayed_trigger::resolve` rewrites it to
/// `After(creation_turn)`, mirroring the existing `PlayerId(0) -> controller`
/// rewrite in the same block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TurnGate {
    /// No floor — fire at the nearest matching phase (Greasefang "your next end
    /// step" may be the current turn). DEFAULT; existing users keep this.
    #[default]
    None,
    /// Parse-time symbolic "no earlier than the creating player's next turn"
    /// (Kav Landseeker "the end step on your next turn"). Rewritten to `After`
    /// at resolution.
    AfterCreationTurn,
    /// Resolve-stamped concrete floor: fire only on a turn strictly greater.
    After(u32),
}

impl TurnGate {
    /// Serde skip-helper: `None` is the default and is omitted from JSON, so all
    /// existing serialized card-data stays byte-identical.
    pub fn is_none(&self) -> bool {
        matches!(self, TurnGate::None)
    }
}

/// When a delayed triggered ability fires (CR 603.7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DelayedTriggerCondition {
    /// "at the beginning of the next [phase]"
    /// CR 603.7: fires on next PhaseChanged for that phase.
    AtNextPhase { phase: Phase },
    /// "at the beginning of your next [phase]"
    /// Fires only when the specified player is active.
    AtNextPhaseForPlayer {
        phase: Phase,
        player: PlayerId,
        /// CR 513.2: turn-floor gate for "on your next turn" skip-current
        /// semantics. `None` (default) = fire at the nearest matching phase.
        #[serde(default, skip_serializing_if = "TurnGate::is_none")]
        gate: TurnGate,
    },
    /// "when [object] leaves the battlefield"
    WhenLeavesPlay {
        object_id: super::identifiers::ObjectId,
    },
    /// CR 603.7c: "when [object] dies" — fires on zone change to graveyard.
    /// Filter-based variant resolved at trigger check time (unlike WhenLeavesPlay
    /// which uses a specific object_id).
    WhenDies { filter: TargetFilter },
    /// CR 603.7c: "when [object] leaves the battlefield" — filter-based variant
    /// that fires on any zone change from battlefield.
    WhenLeavesPlayFiltered { filter: TargetFilter },
    /// CR 603.7c: "when [object] enters the battlefield" — fires on zone change
    /// to battlefield.
    WhenEntersBattlefield { filter: TargetFilter },
    /// "when [object] dies or is exiled" — fires on zone change to graveyard OR exile.
    /// Filter-based variant resolved at trigger check time.
    WhenDiesOrExiled { filter: TargetFilter },
    /// CR 603.7c: "Whenever [event] this turn" — fires each time the event occurs
    /// until end of turn. Reuses existing trigger matching infrastructure via embedded
    /// TriggerDefinition. The embedded trigger's `execute` field should be `None` —
    /// the actual effect lives in `DelayedTrigger.ability`.
    WheneverEvent { trigger: Box<TriggerDefinition> },
    /// CR 603.7: "When you next [event] this turn" — fires once on the next matching
    /// event, then is removed. One-shot variant of `WheneverEvent`.
    /// Uses existing trigger matching infrastructure to detect the event.
    WhenNextEvent {
        trigger: Box<TriggerDefinition>,
        /// Optional alternate matcher for disjunctive "when you next … or …"
        /// clauses (Magus Lucea Kane). Either branch satisfies the condition;
        /// only the first matching event fires the delayed ability.
        #[serde(default)]
        or_trigger: Option<Box<TriggerDefinition>>,
        /// CR 603.7b: whether this one-shot trigger has a stated "this turn"
        /// duration (`ThisTurn`, pruned at cleanup if unfired) or no stated
        /// duration (`Persistent`, survives across turns until the event fires —
        /// The Pandorica's untap/leave re-entry).
        #[serde(default, skip_serializing_if = "DelayedTriggerLifetime::is_this_turn")]
        lifetime: DelayedTriggerLifetime,
    },
}

/// Specifies variable-count targeting for "any number of" effects.
/// CR 601.2c: Player chooses targets during resolution.
/// CR 115.1d: "Any number" means zero or more.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiTargetSpec {
    #[serde(
        serialize_with = "serialize_multi_target_min",
        deserialize_with = "deserialize_multi_target_min"
    )]
    pub min: QuantityExpr,
    /// `None` means "any number" (unlimited). CR 115.1d.
    pub max: Option<QuantityExpr>,
}

impl MultiTargetSpec {
    pub fn fixed(min: usize, max: usize) -> Self {
        Self::bounded_expr(
            QuantityExpr::Fixed { value: min as i32 },
            QuantityExpr::Fixed { value: max as i32 },
        )
    }

    pub fn up_to(max: QuantityExpr) -> Self {
        Self::bounded_expr(QuantityExpr::Fixed { value: 0 }, max)
    }

    pub fn exact(count: QuantityExpr) -> Self {
        Self::bounded_expr(count.clone(), count)
    }

    pub fn unlimited(min: usize) -> Self {
        Self {
            min: QuantityExpr::Fixed { value: min as i32 },
            max: None,
        }
    }

    pub fn bounded(min: usize, max: QuantityExpr) -> Self {
        Self::bounded_expr(QuantityExpr::Fixed { value: min as i32 }, max)
    }

    pub fn bounded_expr(min: QuantityExpr, max: QuantityExpr) -> Self {
        Self {
            min,
            max: Some(max),
        }
    }

    pub fn min_is_fixed_zero(&self) -> bool {
        matches!(self.min, QuantityExpr::Fixed { value: 0 })
    }

    pub fn fixed_min_usize(&self) -> Option<usize> {
        match self.min {
            QuantityExpr::Fixed { value } if value >= 0 => Some(value as usize),
            _ => None,
        }
    }

    pub fn map_quantities<F>(&mut self, mut map: F)
    where
        F: FnMut(QuantityExpr) -> QuantityExpr,
    {
        self.min = map(self.min.clone());
        if let Some(max) = self.max.take() {
            self.max = Some(map(max));
        }
    }
}

fn serialize_multi_target_min<S>(min: &QuantityExpr, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match min {
        QuantityExpr::Fixed { value } => serializer.serialize_i32(*value),
        other => other.serialize(serializer),
    }
}

fn deserialize_multi_target_min<'de, D>(deserializer: D) -> Result<QuantityExpr, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum MultiTargetMin {
        Fixed(i32),
        Expr(QuantityExpr),
    }

    match MultiTargetMin::deserialize(deserializer)? {
        MultiTargetMin::Fixed(value) => Ok(QuantityExpr::Fixed { value }),
        MultiTargetMin::Expr(expr) => Ok(expr),
    }
}

// ---------------------------------------------------------------------------
// TargetFilter -- replaces TargetSpec entirely
// ---------------------------------------------------------------------------

/// Type filter for card type matching in filters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TypeFilter {
    Creature,
    Land,
    Artifact,
    Enchantment,
    Instant,
    Sorcery,
    Planeswalker,
    /// CR 310: Battle — a permanent type introduced in March of the Machine.
    Battle,
    /// CR 308.1: Kindred — each kindred card has another card type; matched via CoreType::Kindred.
    Kindred,
    Permanent,
    Card,
    Any,
    /// CR 205.4b: Negation — matches objects whose type does NOT match the inner filter.
    /// "noncreature" → `Non(Box::new(Creature))`, "non-Human" → `Non(Box::new(Subtype("Human")))`
    Non(Box<TypeFilter>),
    /// CR 205.3: Matches objects with a specific subtype (creature type, land type, etc.).
    /// String because MTG has 250+ creature subtypes (CR 205.3m) with new ones each set.
    Subtype(String),
    /// CR 608.2b: Disjunction — matches if ANY inner filter matches.
    /// "creature or enchantment" → `AnyOf(vec![Creature, Enchantment])`
    AnyOf(Vec<TypeFilter>),
}

/// Filter for damage type on trigger definitions.
/// CR 120.3: Combat damage is dealt during the combat damage step; all other damage is noncombat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum DamageKindFilter {
    /// Matches both combat and noncombat damage.
    #[default]
    Any,
    /// CR 120.2a: Only combat damage (dealt as a result of combat).
    CombatOnly,
    /// CR 120.2b: Only noncombat damage (dealt as an effect of a spell or ability).
    NoncombatOnly,
}

/// CR 120.6 + CR 120.10: Which damage tally a damage-history reference reads —
/// the total amount dealt (CR 120.6) or only the excess beyond lethal
/// (CR 120.10). Typed replacement for the former `excess_only: bool` on
/// `QuantityRef::DamageDealtThisTurn`; a `bool` couldn't name the two channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum DamageChannel {
    /// CR 120.6: Total damage marked (lethal = total marked ≥ toughness). The
    /// default channel for every quantity/condition that does not restrict to
    /// overkill.
    #[default]
    Total,
    /// CR 120.10: Only the excess damage dealt beyond lethal/loyalty/defense —
    /// "excess damage equal to the difference". Used by the "was dealt excess
    /// damage this way / this turn" condition class.
    Excess,
}

/// Serde guard: the `Total` default channel is elided from serialized output,
/// mirroring the prior `skip_serializing_if = "std::ops::Not::not"` on the bool.
fn is_total_damage_channel(channel: &DamageChannel) -> bool {
    matches!(channel, DamageChannel::Total)
}

/// CR 120.2a: Back-compat serde default for `PlayerFilter::OpponentDealtDamage`'s
/// `kind` field. Legacy data serialized before the field existed encoded the
/// former `OpponentDealtCombatDamage` variant (combat-only semantics), so an
/// absent `kind` must deserialize to `CombatOnly` — NOT `DamageKindFilter`'s own
/// `Any` default, which would silently broaden the filter to noncombat damage
/// too and misresolve reloaded games/scenarios.
fn damage_kind_combat_only() -> DamageKindFilter {
    DamageKindFilter::CombatOnly
}

/// CR 120.6 + CR 120.10: Compatibility deserializer for the `channel` field that
/// replaced the former `excess_only: bool` on `QuantityRef::DamageDealtThisTurn`
/// (and `AbilityCondition::PreviousEffectAmount`). Accepts both the current
/// `DamageChannel` representation ("Total" / "Excess") and the legacy bool
/// (`true` → `Excess`, `false` → `Total`), so a game state / scenario serialized
/// before the migration reloads with the correct channel instead of silently
/// defaulting to a total-damage check. Paired with `#[serde(alias =
/// "excess_only")]` on the field, which routes the legacy key's value here.
fn deserialize_damage_channel_compat<'de, D>(d: D) -> Result<DamageChannel, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize as _;
    let raw = serde_json::Value::deserialize(d)?;
    if let serde_json::Value::Bool(excess_only) = raw {
        return Ok(if excess_only {
            DamageChannel::Excess
        } else {
            DamageChannel::Total
        });
    }
    serde_json::from_value(raw).map_err(serde::de::Error::custom)
}

/// CR 608.2n: Compatibility deserializer for the `graveyard_replacement` field
/// that replaced the former `exile_instead_of_graveyard_on_resolve: bool` on
/// `CastingPermission::ExileWithAltCost`. Accepts both the current
/// `Option<SpellStackToGraveyardReplacement>` representation and the legacy bool
/// (`true` → `Some(Exile)`, the only destination the bool could encode; `false`
/// → `None`), so an in-flight saved / reconnected permission serialized before
/// the migration still reloads with its exile-on-resolution rider. Paired with
/// `#[serde(alias = "exile_instead_of_graveyard_on_resolve")]` on the field.
fn deserialize_graveyard_replacement_compat<'de, D>(
    d: D,
) -> Result<Option<SpellStackToGraveyardReplacement>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize as _;
    let raw = serde_json::Value::deserialize(d)?;
    if let serde_json::Value::Bool(exile) = raw {
        return Ok(exile.then_some(SpellStackToGraveyardReplacement::Exile));
    }
    serde_json::from_value(raw).map_err(serde::de::Error::custom)
}

/// Controller reference for filter matching.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControllerRef {
    You,
    Opponent,
    /// CR 115.10 + CR 608.2c: Filter controller is the current player being
    /// affected by an "each player/opponent" instruction during resolution.
    ScopedPlayer,
    /// CR 109.4 + CR 115.1: Filter controller is the player chosen as a target
    /// of the enclosing ability (e.g., "each creature target player controls").
    /// At resolution time, `filter_inner` reads the first `TargetRef::Player`
    /// from `ability.targets`. At target-selection time, `collect_target_slots`
    /// surfaces a companion `TargetFilter::Player` slot so the player is chosen
    /// as part of CR 601.2c / CR 603.3d target declaration.
    TargetPlayer,
    // LEGALITY-SCOPE PAIR: TargetPlayer (any) and TargetOpponent (opponent-only)
    // are runtime-read-identical; they differ ONLY in the companion slot's legal
    // targets. A THIRD legality-scoped Target* variant MUST NOT be added as a
    // sibling — at 3 it crosses the /add-engine-variant Stage-2 threshold: refactor
    // to `TargetPlayer { constraint: PlayerRelation }` instead.
    /// CR 109.4 + CR 102.2 / CR 102.3: filter controller is the single OPPONENT
    /// chosen as a target of the enclosing ability. Runtime read is identical to
    /// `TargetPlayer` (first `TargetRef::Player` in `ability.targets`); the sole
    /// difference is the companion slot's legal-target set
    /// (`Typed{controller:Opponent}` → `find_legal_targets` excludes self).
    TargetOpponent,
    /// CR 608.2c + CR 109.4: Filter controller is the controller of the parent
    /// object target inherited by this chained effect ("that permanent's
    /// controller may sacrifice a land").
    ParentTargetController,
    /// CR 608.2c + CR 108.3: Filter owner is the owner of the parent object
    /// target inherited by this chained effect ("its owner's graveyard").
    ParentTargetOwner,
    /// CR 508.5 / CR 508.5a: Filter controller is the defending player for
    /// the source attacking creature, resolved per attacker through
    /// `combat::defending_player_for_attacker`. Used by intervening-if
    /// quantity checks such as "defending player controls more lands than you."
    DefendingPlayer,
    /// CR 608.2c + CR 109.4: Filter controller is the player chosen by the
    /// Nth `Effect::Choose { choice_type: ChoiceType::Player }` in this
    /// resolving ability chain (`index` is 0-based: 0 = the first choose).
    /// Distinct from `TargetPlayer` (a target declared when the ability went
    /// on the stack): a chosen player is selected *during* resolution via the
    /// `WaitingFor::NamedChoice` round-trip. Resolved by reading
    /// `ResolvedAbility.chosen_players[index]`, the resolution-scoped list the
    /// `NamedChoice` answer handler appends to. Powers the
    /// "choose a player. They <verb> … choose a second player to <verb>"
    /// card class (Gluntch, the Bestower; the Tempt cycle).
    ChosenPlayer {
        index: u8,
    },
    /// CR 613.1 + CR 109.4: Filter controller is the player PERSISTED on the
    /// source via `ChosenAttribute::Player` — the player chosen by an
    /// "as ~ enters the battlefield, choose a player" replacement. Read at
    /// filter / layer-evaluation time from the source's `chosen_attributes`
    /// (mirrors `GameObject::protector`). Distinct from `ChosenPlayer { index }`,
    /// which is resolution-scoped (valid only mid-resolution); this is a durable
    /// characteristic readable continuously, as a CDA requires. Powers
    /// "~'s power and toughness are each equal to the number of <X> the chosen
    /// player controls" (Skyshroud War Beast, Lost Order of Jarkeld).
    SourceChosenPlayer,
    /// CR 603.2 + CR 109.4: Filter controller is the player identified by the
    /// triggering event (the drawer, life-gainer, attacker, etc.). Resolved
    /// against `state.current_trigger_event` via `extract_player_from_event`.
    /// Mirrors `PlayerFilter::TriggeringPlayer` and `TargetFilter::TriggeringPlayer`.
    /// Used by control-relative trigger restrictions
    /// ("an opponent who controls F draws a card").
    TriggeringPlayer,
    /// CR 303.4b + CR 702.5a: Filter controller is the player the source Aura
    /// is attached to ("enchanted player controls"). Resolved at runtime by
    /// reading `source.attached_to` and extracting the `PlayerId` via
    /// `AttachTarget::as_player`. Powers the Curse cycle (Trespasser's Curse,
    /// Curse of Clinging Webs, Curse of the Restless Dead) where the trigger
    /// watches objects controlled by the enchanted player.
    EnchantedPlayer,
    /// CR 102.1: Filter controller is the active player — the player whose turn
    /// it is. Exactly one player at any time (unlike `Opponent`, which matches
    /// every opponent in multiplayer). Resolved live from `state.active_player`
    /// via `controller_ref_player`. Powers "the active player controls" subjects
    /// and the card-assembly-bound punisher target on cards that coerce the turn
    /// player (Siren's Call, Maddening Imp), cast/activated only during an
    /// opponent's turn.
    ActivePlayer,
}

/// CR 301 / CR 303: Kinds of attachments to permanents.
/// Used by `FilterProp::HasAttachment` and `QuantityRef::AttachmentsOnLeavingObject`
/// to parameterize attachment-predicate checks.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AttachmentKind {
    /// CR 303.4: Aura — enchantment subtype that attaches via Enchant ability.
    Aura,
    /// CR 301.5: Equipment — artifact subtype that attaches via Equip ability.
    Equipment,
}

/// Qualities that can be shared across multi-target selections.
/// Used by `FilterProp::SharesQuality` for group constraint validation at resolution time.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SharedQuality {
    /// CR 201.2: object names compared by shared name.
    Name,
    /// CR 202.3: mana value.
    ManaValue,
    /// CR 208.1: power.
    Power,
    /// CR 208.1: toughness.
    Toughness,
    /// CR 208.1: sum of power and toughness.
    TotalPowerToughness,
    CreatureType,
    Color,
    CardType,
    /// CR 205.3i + CR 305.6: land subtypes, including the five basic land types.
    LandType,
    /// CR 110.4: the six permanent types only — artifact, battle, creature,
    /// enchantment, land, planeswalker. Distinct from `CardType` (CR 205.2a),
    /// which also includes non-permanent types such as Kindred/Tribal; two
    /// permanents sharing only Kindred do NOT share a permanent type.
    PermanentType,
}

/// Relationship required by `FilterProp::SharesQuality`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SharedQualityRelation {
    #[default]
    Shares,
    DoesNotShare,
}

fn is_default_shared_quality_relation(value: &SharedQualityRelation) -> bool {
    matches!(value, SharedQualityRelation::Shares)
}

/// Combat relationship required by `FilterProp::CombatRelation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CombatRelation {
    /// CR 509.1g/509.1h: Candidate is blocking the subject or is blocked by it.
    BlockingOrBlockedBy,
}

/// Context object for a combat relationship filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CombatRelationSubject {
    /// The source object of the resolving spell or ability.
    Source,
    /// The first selected object target of the resolving spell or ability.
    ParentTarget,
}

/// Individual filter properties that can be combined in a Typed filter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum FilterProp {
    /// CR 111.1: Matches objects that are tokens.
    Token,
    /// CR 111.1: Matches objects that are not tokens.
    NonToken,
    /// CR 108.2 + CR 108.2b: Matches an object represented by a Magic card,
    /// excluding tokens and object copies. This is distinct from `NonToken`:
    /// a non-token copy is not represented by a card.
    RepresentedByCard,
    /// CR 607.2d / CR 607.2m (by analogy) + CR 611.2c: matches objects whose
    /// CONTROLLER's durable per-player choice records the anchor `label`
    /// ("creatures controlled by players who last chose red waterfall …", Two
    /// Streams Facility). Evaluated via `game::players::player_last_chose_label`
    /// against `obj.controller`. Because the affected set is recomputed each layer
    /// pass (CR 611.2c), a creature entering under a red-waterfall controller
    /// joins the buff and one leaving drops it. Object-axis mirror of the
    /// player-axis `TargetFilter::PlayerWhoChoseLabel`.
    ControllerChoseLabel {
        label: String,
    },
    /// CR 109.4 + CR 608.2c + CR 611.2c: Matches objects whose CONTROLLER
    /// satisfies an inner `PlayerFilter` predicate — the object-axis
    /// generalization of `ControllerChoseLabel` (which hard-codes a single
    /// chosen-label predicate). Evaluated via the single-authority
    /// `matches_player_scope` against `obj.controller`, so ANY player predicate
    /// ("who was dealt combat damage by a Pirate this turn", "who lost life this
    /// turn", "who controls an artifact", …) can scope a target's controller.
    /// `Box` breaks the FilterProp→PlayerFilter→…→TargetFilter→FilterProp size
    /// cycle (mirrors other boxed inner filters). Object-axis mirror of the
    /// player-axis PlayerFilter enum. Admiral Beckett Brass (controller
    /// look-back predicate on the steal target).
    ControllerMatches {
        player: Box<PlayerFilter>,
    },
    /// CR 305.1 + CR 601.2a: Matches objects entering from being played
    /// (land play) or cast (spell), excluding tokens put directly onto the
    /// battlefield without a prior zone.
    WasPlayed,
    /// CR 508.1b: Matches attacking creatures, optionally scoped by which player
    /// the creature is attacking.
    Attacking {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        defender: Option<ControllerRef>,
    },
    /// CR 509.1a: Matches creatures that are blocking.
    Blocking,
    /// CR 509.1g: Matches creatures currently blocking the filter source.
    /// Used for "creature(s) blocking it" source-relative quantities and filters.
    BlockingSource,
    /// CR 509.1g/509.1h: Matches creatures in a combat relationship with a
    /// source or selected parent target.
    CombatRelation {
        relation: CombatRelation,
        subject: CombatRelationSubject,
    },
    /// CR 509.1h: Matches attacking creatures with no blockers assigned.
    Unblocked,
    /// CR 506.5: Matches a creature that is (or, via the zone-change look-back
    /// snapshot, was) the sole attacker — "attacking alone". Live evaluation
    /// reads combat; look-back evaluation reads
    /// `ZoneChangeCombatStatus::attacking_alone`.
    AttackingAlone,
    /// CR 506.5: Matches a creature that is (or was) the sole blocker —
    /// "blocking alone". Look-back evaluation reads
    /// `ZoneChangeCombatStatus::blocking_alone`.
    BlockingAlone,
    Tapped,
    /// CR 302.6 / CR 110.5: Untapped status as targeting qualifier.
    Untapped,
    /// CR 702.171b: Matches permanents with the saddled designation.
    IsSaddled,
    /// CR 702.171c: Matches a creature that saddled the filter source this turn
    /// (i.e. was tapped to pay the source's saddle cost — recorded in the
    /// source's `saddled_by` and cleared at end of turn). Source-relative sibling
    /// of `BlockingSource`; used by "choose a creature that saddled it this turn"
    /// (Calamity, Galloping Inferno).
    SaddledSource,
    /// CR 702.51c: Matches a creature that convoked the filter source — i.e. was
    /// tapped to pay the source spell's convoke cost, recorded in the source
    /// object's `convoked_creatures`. Source-relative, mirroring `SaddledSource`
    /// / `BlockingSource`. Membership analogue of
    /// `QuantityRef::ConvokedCreatureCount` (both read `convoked_creatures`).
    /// Used by "a creature that convoked this spell" (Everything Comes to Dust).
    ConvokedSource,
    /// CR 310.8a + CR 310.8e: Matches battles whose protector satisfies
    /// `controller` relative to the ability source ("each battle they protect").
    ProtectorMatches {
        controller: ControllerRef,
    },
    /// CR 302.6 + CR 702.10b + CR 702.154a: Matches creatures that either have
    /// haste or have been under their controller's control continuously since
    /// that player's most recent turn began. Used by Enlist's tap eligibility.
    HasHasteOrControlledSinceTurnBegan,
    WithKeyword {
        value: Keyword,
    },
    HasKeywordKind {
        value: KeywordKind,
    },
    /// CR 702: Matches objects that do NOT have a given keyword ability.
    /// Used for "without flying", "without first strike", etc.
    WithoutKeyword {
        value: Keyword,
    },
    WithoutKeywordKind {
        value: KeywordKind,
    },
    /// CR 303.4 + CR 702.5: Matches Aura objects whose enchant ability can
    /// legally enchant the referenced target. This is a semantic predicate,
    /// not exact keyword equality: `Enchant(creature)` can satisfy "that could
    /// enchant that creature" where the reference resolves through the
    /// ability's target slots.
    CanEnchant {
        target: Box<TargetFilter>,
    },
    /// CR 122.1: Matches objects whose counter count satisfies `comparator`
    /// against `count`. `counters` selects which counters are counted —
    /// `CounterMatch::OfType(t)` counts only type `t`, `CounterMatch::Any` sums
    /// across all counter types. Replaces the legacy `CountersGE` +
    /// `HasAnyCounter` variants; the comparator axis is parameterized via
    /// `Comparator`, unlocking EQ/LT/NE ("without a +1/+1 counter", "with no
    /// counters") without sibling proliferation — mirrors the `Cmc` refactor.
    Counters {
        counters: CounterMatch,
        comparator: Comparator,
        count: QuantityExpr,
    },
    /// Matches objects whose mana value satisfies `comparator` against `value`.
    /// CR 202.3. Replaces the legacy `CmcGE`/`CmcLE`/`CmcEQ` sibling cluster;
    /// the comparator axis is parameterized via the existing `Comparator` enum,
    /// unlocking GT/LT/NE comparisons without further variant proliferation.
    /// Supports both fixed and dynamic (e.g., X) thresholds via `QuantityExpr`.
    Cmc {
        comparator: Comparator,
        value: QuantityExpr,
    },
    /// CR 202.3 + CR 608.2c: Matches objects whose mana value has the selected
    /// odd/even quality, either fixed or chosen earlier in the same resolving
    /// instruction sequence.
    ManaValueParity {
        parity: ParitySource,
    },
    /// CR 202.1: Matches objects whose printed mana cost is exactly one of `costs`.
    /// Distinct from `Cmc`/mana value (CR 202.3): "{0} or {1}" must not match
    /// artifacts with colored one-mana costs like {W}.
    ManaCostIn {
        costs: Vec<ManaCost>,
    },
    InZone {
        zone: Zone,
    },
    Owned {
        controller: ControllerRef,
    },
    /// CR 702.143c-d: Matches cards in exile that have the foretold designation.
    /// This is a status of the exiled card, not equivalent to having the
    /// Foretell keyword or a generic exile casting permission.
    Foretold,
    /// CR 715.2: Matches a card that has an Adventure. Outside the stack, an
    /// Adventure card has only the characteristics of its permanent half, so
    /// this must inspect the stored alternate face rather than its card types.
    HasAdventure,
    EnchantedBy,
    EquippedBy,
    /// CR 301.5 + CR 303.4: True when the matched object's `attached_to` field
    /// equals the filter source's object ID. Inverse of `EnchantedBy`/`EquippedBy`,
    /// which check whether the source has an attachment. Used for "Aura and
    /// Equipment attached to ~" quantity clauses (Kellan, the Fae-Blooded) and
    /// for any compound filter whose subject is "attached to <self>".
    AttachedToSource,
    /// CR 301.5 + CR 303.4 + CR 613.4c: True when the matched object's
    /// `attached_to` field equals the *recipient* of the resolving effect (the
    /// per-object `id` in a layer's affected list). Distinct from
    /// `AttachedToSource` — for an Aura/Equipment static "Enchanted/Equipped
    /// creature gets +N/+M for each X attached to it", the pronoun "it" refers
    /// to the affected creature, not to the static's source object. The
    /// recipient is supplied through `FilterContext::recipient_id`; when
    /// recipient is unknown (no per-recipient context), this prop evaluates to
    /// false. Covers ~25 cards: Strong Back, Mantle of the Ancients,
    /// Auramancer's Guise, Champion of the Flame, Bruenor Battlehammer's
    /// "Each creature you control gets +2/+0 for each Equipment attached to
    /// it", and the broader "<subject> gets +N/+M for each Aura/Equipment
    /// attached to it" family.
    AttachedToRecipient,
    /// CR 303.4 + CR 301.5: Matches objects that have at least one attachment of the
    /// given kind whose controller matches `controller`. Unlike `EnchantedBy`/`EquippedBy`
    /// (which are source-relative — match when THIS source is attached to the object),
    /// this predicate is non-source-relative by default: it matches any object with a
    /// qualifying attachment. `exclude_source` preserves "another Aura/Equipment"
    /// semantics for Aura legality so the source attachment cannot satisfy its own
    /// "another" restriction after it becomes attached. `controller = None` means
    /// "any controller".
    ///
    /// Covers:
    /// - "enchanted creature" when the ability source is not the Aura itself
    ///   (e.g. Hateful Eidolon's "Whenever an enchanted creature dies, ...").
    /// - "creature enchanted by an Aura you control" (Killian, Decisive Mentor).
    /// - "creature equipped by an Equipment you control" (future).
    HasAttachment {
        kind: AttachmentKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        controller: Option<ControllerRef>,
        #[serde(
            default,
            with = "source_exclusion_bool_compat",
            skip_serializing_if = "SourceExclusion::is_include"
        )]
        exclude_source: SourceExclusion,
    },
    /// CR 303.4 + CR 301.5: Disjunctive attachment predicate — matches objects that
    /// have at least one attachment whose subtype is in `kinds` and whose controller
    /// satisfies the optional `ControllerRef`. Generalizes `HasAttachment` to the
    /// "enchanted or equipped" compound subject class (Reyav, Master Smith;
    /// Dogmeat, Ever Loyal). Single-kind use is also valid (kinds.len() == 1) but
    /// `HasAttachment` is preferred for that case.
    HasAnyAttachmentOf {
        kinds: Vec<AttachmentKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        controller: Option<ControllerRef>,
    },
    /// Matches any object that is NOT the trigger source (for "another creature" triggers).
    Another,
    /// CR 702.95b: Matches objects that are not paired with another creature.
    Unpaired,
    /// CR 603.4 + CR 109.3: Matches any object that is NOT the object that caused
    /// the currently-evaluating trigger to fire (the "triggering object"). Distinct
    /// from `Another`, which excludes the *ability source*. Used for intervening-if
    /// count predicates like Valakut, the Molten Pinnacle's "if you control at
    /// least five other Mountains" — here "other" means "other than the newly-
    /// entered Mountain," not "other than Valakut." Resolves against
    /// `FilterContext::triggering_object_id`, populated at trigger-condition
    /// evaluation from the current `GameEvent`.
    OtherThanTriggerObject,
    /// Matches objects with a specific color (for "white creature", "red spell", etc.).
    HasColor {
        color: ManaColor,
    },
    /// CR 208 (Power/Toughness) + CR 208.4b (base vs current) + CR 613.4b
    /// (layer 7b: base P/T is set before counters/modifiers in 7c): Matches
    /// objects whose power, toughness, or total power and toughness satisfies
    /// `comparator` against `value`.
    ///
    /// Replaces the former `PowerLE`/`PowerGE`/`ToughnessLE`/`ToughnessGE`
    /// sibling cluster (a `stat × comparator` cross-product) with a single
    /// parameterized predicate, mirroring the `Cmc` and `Counters` refactors.
    /// Three orthogonal axes:
    /// - `stat`: which P/T metric to read — power (CR 208.1), toughness, or
    ///   their total.
    /// - `scope`: `Current` reads the live `power`/`toughness` (after all layers);
    ///   `Base` reads `base_power`/`base_toughness` per CR 208.4b — the value
    ///   after CDAs and set effects (layers 7a/7b) but ignoring counters and
    ///   modifying effects (layer 7c). A 1/1 with a +1/+1 counter has base
    ///   power 1 but current power 2.
    /// - `comparator`: any `Comparator` (LE/GE/EQ/LT/GT/NE).
    ///
    /// The disjunctive natural-language form "power or toughness N or less"
    /// composes two `PtComparison` props under `AnyOf`.
    ///
    /// Card data is regenerated from Oracle text at build time, so no legacy-tag
    /// deserialization shim is required; `scope` defaults to `Current` when
    /// absent for forward-compatible hand-authored fixtures.
    PtComparison {
        stat: PtStat,
        #[serde(default)]
        scope: PtValueScope,
        comparator: Comparator,
        value: QuantityExpr,
    },
    /// CR 509.1b: Matches objects whose power is strictly greater than the source object's power.
    /// Used for "creatures with greater power" blocking restrictions (relative comparison).
    PowerGTSource,
    /// CR 105.2: Matches objects by color-set size. Covers colorless
    /// (0 colors), monocolored (exactly 1 color), and multicolored
    /// (2 or more colors) without proliferating sibling variants.
    ColorCount {
        comparator: Comparator,
        count: u8,
    },
    /// CR 107.4 + CR 202.1: Matches objects whose count of colored mana symbols
    /// in the printed mana cost satisfies `comparator` against `value`.
    /// `color: Some(c)` counts symbols contributing to color `c` (hybrid /
    /// monocolored-hybrid / Phyrexian via `ManaCostShard::contributes_to`,
    /// CR 107.4a/107.4e/107.4f); `color: None` counts each colored symbol once.
    /// `value` is a printed constant. Filter sibling of
    /// `QuantityRef::ManaSymbolsInManaCost`; both delegate the pip count to the
    /// single counting authority `ManaCost::count_colored_pips`.
    ManaSymbolCount {
        color: Option<ManaColor>,
        comparator: Comparator,
        value: i32,
    },
    /// Matches objects with a specific supertype (Basic, Legendary, Snow).
    HasSupertype {
        value: Supertype,
    },
    /// Matches objects whose subtypes include the source object's chosen creature type.
    /// Used for "of the chosen type" patterns (Cavern of Souls, Metallic Mimic).
    IsChosenCreatureType,
    /// CR 205.3m + CR 701.23a: Matches creature cards whose creature type is
    /// tied for the highest count among creature cards in the named player's
    /// named zone. CR 205.3m defines the creature subtype set being counted;
    /// CR 701.23a is the search mechanic that surfaces this filter.
    /// Generalizes the original "most prevalent creature type in your library"
    /// leaf so future variants (opponent's library, graveyard, etc.) reuse
    /// this slot rather than spawning siblings.
    ///
    /// Backward compat: deserializes from the legacy tag
    /// `MostPrevalentCreatureTypeInLibrary` (no fields) via `#[serde(alias)]`,
    /// defaulting `zone = Library` and `scope = You`.
    #[serde(alias = "MostPrevalentCreatureTypeInLibrary")]
    MostPrevalentCreatureTypeIn {
        #[serde(default = "default_most_prevalent_zone")]
        zone: crate::types::zones::Zone,
        #[serde(default = "default_most_prevalent_scope")]
        scope: ControllerRef,
    },
    /// Matches objects whose colors include the source object's chosen color.
    /// Used for "of the chosen color" patterns (Hall of Triumph, Runed Stalactite).
    /// Reads `ChosenAttribute::Color` from the source permanent.
    IsChosenColor,
    /// Matches objects whose core type includes the source object's chosen card type.
    /// Used for "spells of the chosen type" patterns (Archon of Valor's Reach).
    /// Reads `ChosenAttribute::CardType` from the source permanent.
    IsChosenCardType,
    /// CR 205.2 + CR 608.2c: Matches objects by a transient card predicate
    /// choice made earlier in the same resolving instruction sequence. Used by
    /// "of the chosen kind" library filters and guess-resolution conditions.
    MatchesLastChosenCardPredicate,
    /// CR 115.7: Matches stack entries that have exactly one target.
    /// Used for "with a single target" qualifiers on retarget effects.
    HasSingleTarget,
    /// CR 700.2: Matches a spell/ability on the stack that is modal (its printed
    /// text offers two or more options in a bulleted list). Evaluated against the
    /// stack object's static printed modality (`obj.modal.is_some()`), a printed
    /// characteristic present from object creation.
    Modal,
    /// CR 205.4b: Matches objects that do NOT have a specific color.
    /// Parallel to `HasColor` — used for "nonblack", "nonwhite" in negation stacks.
    NotColor {
        color: ManaColor,
    },
    /// CR 205.4a: Matches objects that do NOT have a specific supertype.
    /// Parallel to `HasSupertype` — used for "nonbasic", "nonlegendary" in negation stacks.
    NotSupertype {
        value: Supertype,
    },
    /// CR 701.60b: Matches suspected creatures.
    Suspected,
    /// CR 702.112b: Matches permanents with the renowned designation.
    Renowned,
    /// CR 510.1c: Matches creatures whose toughness is greater than their power.
    ToughnessGTPower,
    /// CR 208.1 + CR 613.4a + CR 613.4b: Matches a creature whose current
    /// (post-layer) power exceeds its base power. Base power is established by
    /// layer 7a CDAs (CR 613.4a) and layer 7b set effects (CR 613.4b), before
    /// the counters and pumps applied in layers 7c–7e. True for a creature
    /// pumped above its base by +1/+1 counters, auras, or anthems; false when
    /// power == base or power < base. Consolidation target if a 3rd same-object
    /// P/T self-comparison appears: `PtSelfComparison { lhs, comparator, rhs }`.
    PowerExceedsBase,
    /// Disjunctive composite: the object matches if ANY inner prop matches.
    /// Used for natural-language OR within a property suffix — e.g.
    /// "creature with power or toughness N or less" decomposes to
    /// `AnyOf { [PtComparison(Power,LE,N), PtComparison(Toughness,LE,N)] }` on a `creature` typed filter,
    /// preserving the single-type constraint while expressing the OR
    /// semantics at the property layer. Nest by composing with other props.
    AnyOf {
        props: Vec<FilterProp>,
    },
    /// CR 608.2c: Logical negation of a filter property — matches objects for
    /// which the inner property does NOT hold ("apply the rules of English").
    /// General recursive combinator mirroring `TargetFilter::Not`,
    /// `StaticCondition::Not`, `AbilityCondition::Not`, and `TriggerCondition::Not`.
    /// Composes with the AND-combined `properties` vector, so a negated-verb
    /// relative clause like "that didn't attack or enter this turn" decomposes
    /// (De Morgan) into `Not(AttackedThisTurn)` AND `Not(EnteredThisTurn)` rather
    /// than a bespoke `NotAttacked`/`NotEntered` sibling cluster. Boxed for recursion.
    Not {
        prop: Box<FilterProp>,
    },
    /// CR 608.2c: The object is a member of the active resolution-chain tracked
    /// set ("... chosen this way" / "the rest"). Composes with `FilterProp::Not`
    /// for "all other <type>" after a choose-into-tracked-set head (Day of the
    /// Doctor IV: "Choose up to three Doctors. You may exile all other
    /// creatures."). The sentinel `TrackedSetId(0)` is resolved to the active
    /// chain set at match time (see `matches_filter_prop` in `game/filter.rs`),
    /// so no compile-time id binding is required. This is a *property* form of
    /// the whole-filter `TargetFilter::TrackedSet` selector — usable as a
    /// negatable constraint inside a `Typed` filter's `properties` vector,
    /// which the whole-filter selector cannot express.
    InTrackedSet {
        id: super::identifiers::TrackedSetId,
    },
    /// CR 700.9: A permanent is modified if it has one or more counters on it
    /// (CR 122), if it is equipped (CR 301.5), or if it is enchanted by an Aura
    /// that is controlled by that permanent's controller (CR 303.4).
    ///
    /// Modeled as a first-class typed predicate rather than an `AnyOf`
    /// composite because CR 700.9 names "modified" as a distinct concept and
    /// the three legs share a single runtime match arm. Parser dispatch emits
    /// `FilterProp::Modified` for "modified creature(s)" subjects, analogous
    /// to how `FilterProp::Suspected` models CR 701.60b's "suspected" status.
    Modified,
    /// CR 700.6: An object is historic if it has the legendary supertype, the
    /// artifact card type, or the Saga subtype.
    ///
    /// Modeled as a first-class typed predicate rather than an `AnyOf`
    /// composite because CR 700.6 names "historic" as a distinct concept and
    /// the three legs share a single runtime match arm. Parser dispatch emits
    /// `FilterProp::Historic` for "historic permanent" / "historic spell" /
    /// "historic card" subjects, mirroring `FilterProp::Modified` for
    /// CR 700.9's "modified" predicate.
    Historic,
    /// CR 700.6: Matches objects that are not historic (negation of `Historic`).
    /// Used for "nonhistoric" / "not historic" in compound type phrases such as
    /// Desynchronization's "nonland, nonhistoric permanent".
    NotHistoric,
    /// Matches objects whose name differs from all objects matching the inner filter
    /// that the evaluating controller controls on the battlefield.
    /// Used for "with a different name than each [type] you control" (e.g. Light-Paws).
    DifferentNameFrom {
        filter: Box<TargetFilter>,
    },
    /// CR 109.1 + CR 120.3: Matches objects that are NOT the same object as any
    /// object the `reference` filter resolves to — the object-identity dual of
    /// [`FilterProp::SharesQuality`] (which carries a `reference` and tests a
    /// shared *quality*; this tests object *identity*). Used for the "each OTHER
    /// <X> that ..." exclusion where "other" is relative to the ability's chosen
    /// target rather than the source: Radiance's "target creature and each other
    /// creature that shares a color with it" (Cleansing Beam) pairs
    /// `DistinctFrom { reference: ParentTarget }` with
    /// `SharesQuality { Color, reference: ParentTarget }` so the fan-out damages
    /// every color-sharer EXCEPT the already-damaged target. Distinct from
    /// [`FilterProp::Another`] (excludes the ability *source*) and
    /// [`FilterProp::OtherThanTriggerObject`] (excludes the *triggering* object).
    DistinctFrom {
        reference: Box<TargetFilter>,
    },
    /// CR 604.3: Matches objects whose current zone is any of the listed zones (OR semantics).
    /// Used for zone-based restrictions like "cards in graveyards and libraries".
    InAnyZone {
        zones: Vec<Zone>,
    },
    /// Multi-target group constraint — all selected targets must share at least
    /// one value of the named quality. Validated at resolution time, not per-object.
    /// Examples: "that share a creature type", "that share a color", "that share a card type".
    SharesQuality {
        quality: SharedQuality,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reference: Option<Box<TargetFilter>>,
        #[serde(default, skip_serializing_if = "is_default_shared_quality_relation")]
        relation: SharedQualityRelation,
    },
    /// CR 510.1: Object was dealt damage during this turn.
    /// Checks `damage_marked > 0` (damage persists until cleanup step).
    WasDealtDamageThisTurn,
    /// CR 120.1: This object *dealt* damage during this turn — i.e. it was the
    /// SOURCE of a damage event ("target creature ... that dealt damage this
    /// turn", Red Guardian, Super-Soldier). The active-voice counterpart of
    /// `WasDealtDamageThisTurn` (which reads the damage recipient). Evaluated by
    /// scanning `state.damage_dealt_this_turn` for a record whose `source_id`
    /// is this object.
    ///
    /// Parameterization note: these two form a damage-role pair. If a third
    /// damage-role filter appears (e.g. "dealt combat damage this turn"), fold
    /// the pair into `DamageThisTurn { role: {Dealt, Received}, combat_only }`
    /// per the /add-engine-variant sibling-cluster threshold.
    DealtDamageThisTurn,
    /// CR 400.7: Object entered the battlefield during this turn.
    /// Checks `entered_battlefield_turn == Some(current_turn)`.
    EnteredThisTurn,
    /// CR 302.6 + CR 508.1a: the object has been under its controller's control
    /// continuously since that player's most recent turn began (haste-INDEPENDENT
    /// — cares only about the continuity limb, unlike attack-eligibility's
    /// haste-OR-continuity). Reads the durable `summoning_sick` flag, set on ETB
    /// and on any control change, cleared at the controller's next turn.
    ControlledContinuouslySinceTurnBegan,
    /// CR 400.7 + CR 700.4: The object moved between matching zones during
    /// this turn. Parameterized for phrases like "cards in your graveyard that
    /// were put there from the battlefield this turn"; `None` on either side
    /// means that side is unconstrained.
    ZoneChangedThisTurn {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<Zone>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<Zone>,
    },
    /// CR 508.1a: Creature was declared as an attacker this turn.
    /// `None` matches a board-wide declaration (any defender), checked against
    /// the `creatures_attacked_this_turn` tracking set on GameState.
    /// CR 508.6 + CR 508.1b: `Some(defender)` scopes to creatures that attacked a
    /// specific player, checked against the per-defender
    /// `creature_attacked_defenders_this_turn` ledger. Mirrors
    /// `FilterProp::Attacking { defender }`.
    AttackedThisTurn {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        defender: Option<ControllerRef>,
    },
    /// CR 509.1a: Creature was declared as a blocker this turn.
    /// Checks `creatures_blocked_this_turn` tracking set on GameState.
    BlockedThisTurn,
    /// CR 508.1a + CR 509.1a: Creature attacked or blocked this turn.
    /// Compound check used by "that attacked or blocked this turn" Oracle text.
    AttackedOrBlockedThisTurn,
    /// CR 122.1 + CR 122.6: Matches an object onto which `actor` put counters
    /// matching `counters` this turn, where the total count satisfies
    /// `comparator` against `count`. This is a *historical-action* predicate
    /// (CR 122.6: "counters being put on an object"), not a current-counter
    /// query — the object stays matched even if those counters are later
    /// removed, which is what distinguishes it from `FilterProp::Counters`.
    /// Evaluated against `GameState::counter_added_this_turn`. Covers the class
    /// "[permanents] that [you've / an opponent has] put [one or more] [+1/+1 /
    /// any] counters on this turn" (Kid Loki's hexproof static, and the broader
    /// counter-this-turn conditional-static class). The `actor` and `counters`
    /// axes mirror `QuantityRef::CounterAddedThisTurn` so both the count and the
    /// membership predicate share one parameterization.
    CountersPutOnThisTurn {
        actor: CountScope,
        counters: crate::types::counter::CounterMatch,
        comparator: Comparator,
        count: u32,
    },
    /// CR 707.2: Matches face-down objects on the battlefield.
    /// Used for "face-down creature" trigger subjects.
    FaceDown,
    /// CR 701.27g: Matches transformed permanents — a transforming double-faced
    /// permanent on the battlefield with its back face up. Used for "transformed
    /// permanent"/"transformed creature" selectors (Mutagen Connoisseur's "+1/+0
    /// for each transformed permanent you control"). Mirrors `FaceDown`/`Tapped`:
    /// a per-object battlefield state read from `GameObject::transformed`.
    Transformed,
    /// CR 115.9c: Matches stack entries whose targets ALL satisfy the given filter.
    /// Used for "that targets only ~", "that targets only a single creature you control", etc.
    /// Permissive at the per-object filter level; validated against the stack entry's actual
    /// targets by trigger matchers and retarget effects.
    TargetsOnly {
        filter: Box<TargetFilter>,
    },
    /// CR 115.9b: Matches stack entries that have at least one target satisfying the filter.
    /// Used for "that targets ~", "that targets you", etc. (.any() semantics).
    /// Contrast with TargetsOnly (CR 115.9c) which requires ALL targets to match (.all()).
    Targets {
        filter: Box<TargetFilter>,
    },
    /// CR 115.1 + CR 707.10: Matches objects that could be chosen as a target of
    /// the triggering spell on the stack. Used for Zada, Hedron Grinder's "for
    /// each other creature you control that the spell could target" copy count.
    CouldBeTargetedByTriggeringSpell,
    /// CR 107.3 + CR 202.1: Matches spells/objects whose printed mana cost contains
    /// an `{X}` shard. Used for "spell with {X} in its mana cost" qualifier on
    /// spell-cast triggers (Lattice Library, Nev the Practical Dean, Owlin
    /// Spiralmancer, Brass Infiniscope, Elementalist's Palette).
    /// Evaluated against `SpellCastRecord.has_x_in_cost` in the spell-history
    /// filter path and against `cost_has_x(&obj.mana_cost)` for live objects.
    HasXInManaCost,
    /// CR 107.3 + CR 602.1: Matches activated abilities whose activation cost
    /// contains an `{X}` shard. Used for "activate an ability with {X} in its
    /// activation cost" on `AbilityActivated` delayed triggers (Magus Lucea Kane).
    HasXInActivationCost,
    /// CR 702.33d: Matches spells whose kicker additional cost was paid for this
    /// cast. Used for "the first kicked spell you cast each turn" cost reducers
    /// (Vine Gecko). Live evaluation reads `pending_cast` / `GameObject.kickers_paid`;
    /// turn-history evaluation reads `SpellCastRecord.was_kicked`.
    WasKicked,
    /// CR 605.1: Matches objects that have at least one ability classified as a
    /// mana ability by the engine's authoritative mana-ability classifier.
    /// Used for library filters such as "artifact card with a mana ability".
    HasManaAbility,
    /// CR 113.1 + CR 113.3: Matches objects that currently have no abilities:
    /// no keyword, activated, triggered, replacement, or static abilities.
    /// Used for library filters such as "creature card with no abilities".
    HasNoAbilities,
    /// CR 201.2: Matches objects whose card name equals the given name.
    /// Used for "cards named [X]" and "named [X]" filter patterns.
    /// Name comparison is exact per CR 201.2a (case-insensitive at evaluation).
    Named {
        name: String,
    },
    /// Matches objects with the same name as a previously-referenced card.
    /// Used for "search your library for a card with that name" patterns.
    SameName,
    /// CR 201.2: Matches objects whose name equals the name of the
    /// resolving ability's first object target. Used by chained sub-abilities
    /// where a prior step targeted/exiled a card and the next step references
    /// "cards with that name" — e.g., Deadly Cover-Up's "search ... for any
    /// number of cards with that name and exile them" (the "that name" is the
    /// name of the card exiled by the immediately preceding effect, which is
    /// inherited as the first target via `TargetFilter::ParentTarget`).
    ///
    /// Differs from `SameName` (which reads the source object's name): this
    /// reads from `ability.targets[0]` when that target is `TargetRef::Object`,
    /// looking up the name from `state.objects` (or `lki_cache` if the target
    /// has already left its zone).
    SameNameAsParentTarget,
    /// CR 201.2 + CR 201.2a: Matches objects whose name equals the name of any
    /// permanent currently on the battlefield. `controller` optionally narrows
    /// the pool of permanents whose names are considered (None = any controller,
    /// i.e. "shares a name with a permanent" unqualified). Used by "put a card
    /// onto the battlefield if it has the same name as a permanent" patterns
    /// (Mitotic Manipulation, and any analogous dig-with-name-match effect).
    NameMatchesAnyPermanent {
        controller: Option<ControllerRef>,
    },
    /// CR 903.3 + CR 903.3d: Matches permanents on the battlefield that are a
    /// commander. Reads `GameObject::is_commander`, set during deck construction
    /// per CR 903.3 (the legendary card designated as that deck's commander).
    /// Used for "commander(s) you control", "your commander" subject phrases
    /// and for "target commander" in commander-format effects (Codsworth, Falthis,
    /// Anara, Champions of Archery, etc.).
    IsCommander,
    /// CR 205.3m + CR 903.3: Matches an object that is a creature AND shares at
    /// least one creature type with the filter-controller's commander(s) — the
    /// Path of Ancestry predicate ("a creature spell that shares a creature type
    /// with your commander").
    ///
    /// RELOCATED, not invented: this exact concept lived on
    /// `ManaRestriction::SharesCreatureTypeWithCommander` (CR 106.6 — SPEND
    /// legality), where it was never a spend restriction at all. Path of
    /// Ancestry's mana may be spent on anything; the predicate only decides
    /// whether the CR 603.3 trigger FIRES. Object predicates belong in
    /// `FilterProp`, so it moves here and the wrong-section variant is deleted.
    ///
    /// Deliberately NOT expressed as `SharesQuality { CreatureType, reference:
    /// <commander> }`, which is the shape one would reach for first. That
    /// reference resolution walks `state.objects` for an `is_commander` object,
    /// but the authority for "your commander" in a live game is
    /// `deck_pools[player].current_commander` — which is exactly why
    /// `commander::commander_creature_types` reads the deck pool FIRST and only
    /// falls back to an object scan. A `SharesQuality` port would consult the
    /// fallback and never the authority, so a commander that is registered but
    /// not instantiated as a flagged object would be invisible and the trigger
    /// would silently stop firing. This variant instead calls
    /// `commander_creature_types` — the same authority the spend site used before
    /// the retype — so the behavior is preserved BY CONSTRUCTION rather than by a
    /// ledger that cannot see runtime evaluation.
    SharesCreatureTypeWithCommander,
    Other {
        value: String,
    },
}

impl FilterProp {
    /// Returns true if `self` and `other` are the same enum variant (ignoring inner values).
    /// Used by `distribute_properties_to_or` to avoid duplicating property kinds.
    pub fn same_kind(&self, other: &Self) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }
}

/// Named fields for the `TargetFilter::Typed` variant, extracted for builder ergonomics.
/// CR 205: `type_filters` holds all type constraints in conjunction (all must match).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypedFilter {
    /// CR 205: All type constraints that must match (conjunction).
    /// e.g. "noncreature, nonland permanent" → `[Permanent, Non(Creature), Non(Land)]`
    #[serde(default)]
    pub type_filters: Vec<TypeFilter>,
    #[serde(default)]
    pub controller: Option<ControllerRef>,
    #[serde(default)]
    pub properties: Vec<FilterProp>,
}

impl TypedFilter {
    pub fn new(card_type: TypeFilter) -> Self {
        Self {
            type_filters: vec![card_type],
            ..Self::default()
        }
    }
    pub fn creature() -> Self {
        Self::new(TypeFilter::Creature)
    }
    pub fn permanent() -> Self {
        Self::new(TypeFilter::Permanent)
    }
    pub fn land() -> Self {
        Self::new(TypeFilter::Land)
    }
    pub fn card() -> Self {
        Self::new(TypeFilter::Card)
    }
    /// Add an additional type constraint (conjunction).
    pub fn with_type(mut self, tf: TypeFilter) -> Self {
        self.type_filters.push(tf);
        self
    }
    pub fn controller(mut self, ctrl: ControllerRef) -> Self {
        self.controller = Some(ctrl);
        self
    }
    /// CR 205.3: Add a subtype constraint (e.g. "Human", "Zombie").
    pub fn subtype(mut self, sub: String) -> Self {
        self.type_filters.push(TypeFilter::Subtype(sub));
        self
    }
    pub fn properties(mut self, props: Vec<FilterProp>) -> Self {
        self.properties = props;
        self
    }

    /// Extract the first subtype from type_filters, if any.
    pub fn get_subtype(&self) -> Option<&str> {
        self.type_filters.iter().find_map(|tf| match tf {
            TypeFilter::Subtype(s) => Some(s.as_str()),
            _ => None,
        })
    }

    /// Extract the primary type filter (first non-Subtype, non-Non entry), if any.
    pub fn get_primary_type(&self) -> Option<&TypeFilter> {
        self.type_filters
            .iter()
            .find(|tf| !matches!(tf, TypeFilter::Subtype(_) | TypeFilter::Non(_)))
    }

    /// Whether this filter has any meaningful type constraint beyond Card/Any.
    pub fn has_meaningful_type_constraint(&self) -> bool {
        self.type_filters
            .iter()
            .any(|tf| !matches!(tf, TypeFilter::Card | TypeFilter::Any))
            || !self.properties.is_empty()
    }

    pub fn normalized(mut self) -> Self {
        self.type_filters = normalized_type_filters(self.type_filters);
        self.properties = normalized_filter_props(self.properties);
        self
    }
}

impl From<TypedFilter> for TargetFilter {
    fn from(f: TypedFilter) -> Self {
        TargetFilter::Typed(f)
    }
}

/// CR 115.1 + CR 701.9b: How a target slot's referent is selected.
///
/// CR 115.1: Default — the spell/ability's controller chooses each target.
/// CR 701.9b: Some effects override the controller-choice default by requiring
/// the game to make the selection (analogous to "random discard" — the affected
/// player does not choose). Magic Oracle text expresses this with the modifier
/// "random target [predicate]" appearing immediately before the target word
/// (Mana Clash, Goblin Lyre, Pixie Queen, Vexing Sphinx, Maddening Hex, etc.).
///
/// Categorical-boundary check (per CLAUDE.md "Parameterize, don't proliferate"):
/// this enum is the *selection-mode* axis — one level above the predicate
/// (`TargetFilter`) and orthogonal to slot count (`MultiTargetSpec`). It does
/// not belong on `TargetFilter` (which captures *what* matches), nor on
/// `MultiTargetSpec` (which captures *how many*). It captures *who selects*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TargetSelectionMode {
    /// CR 115.1: The spell or ability's controller chooses each target.
    #[default]
    Chosen,
    /// CR 701.9b (analogous): The game selects each target uniformly at random
    /// from the legal-target set. The controller does not choose.
    Random,
}

impl TargetSelectionMode {
    /// Helper for `serde(skip_serializing_if = ...)` — the default `Chosen`
    /// mode is omitted from card-data.json so existing card export shapes are
    /// preserved exactly.
    pub fn is_chosen(&self) -> bool {
        matches!(self, TargetSelectionMode::Chosen)
    }

    /// CR 700.2b (override) + CR 701.9b (analogous): The game selects uniformly
    /// at random instead of the controller choosing (Cult of Skaro "choose one
    /// at random").
    pub fn is_random(&self) -> bool {
        matches!(self, TargetSelectionMode::Random)
    }
}

/// CR 701.9a: How cards are selected from a zone during an effect or cost.
///
/// Analogous to `TargetSelectionMode` but for cards from a player's hand (or
/// other non-target zones) rather than spell/ability targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CardSelectionMode {
    /// CR 608.2d: The affected player or controller chooses the card(s).
    #[default]
    Chosen,
    /// CR 701.9a: The game selects card(s) uniformly at random.
    Random,
}

impl CardSelectionMode {
    pub fn is_chosen(&self) -> bool {
        matches!(self, Self::Chosen)
    }

    pub fn is_random(self) -> bool {
        matches!(self, Self::Random)
    }
}

/// Serde adapter for legacy `random: bool` fields in card-data.json.
pub mod card_selection_bool_compat {
    use super::CardSelectionMode;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        mode: &CardSelectionMode,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_bool(mode.is_random())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<CardSelectionMode, D::Error> {
        let random = bool::deserialize(deserializer)?;
        Ok(if random {
            CardSelectionMode::Random
        } else {
            CardSelectionMode::Chosen
        })
    }
}

/// Whether a discard cost discards the ability source itself or cards from hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum DiscardSelfScope {
    /// Discard from hand per `filter` / `count` (the default).
    #[default]
    FromHand,
    /// Discard the source card itself (Channel's "Discard this card").
    SourceCard,
}

impl DiscardSelfScope {
    pub fn is_from_hand(self) -> bool {
        matches!(self, Self::FromHand)
    }

    pub fn is_source_card(self) -> bool {
        matches!(self, Self::SourceCard)
    }
}

/// Serde adapter for legacy `self_ref: bool` on `AbilityCost::Discard`.
pub mod discard_self_scope_bool_compat {
    use super::DiscardSelfScope;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        scope: &DiscardSelfScope,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_bool(scope.is_source_card())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<DiscardSelfScope, D::Error> {
        let self_ref = bool::deserialize(deserializer)?;
        Ok(if self_ref {
            DiscardSelfScope::SourceCard
        } else {
            DiscardSelfScope::FromHand
        })
    }
}

/// Whether an optional or kicker additional cost may be paid multiple times.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum AdditionalCostRepeatability {
    /// Pay at most once (ordinary kicker / optional cost).
    #[default]
    Once,
    /// Pay any number of times (multikicker, replicate).
    Repeatable,
}

impl AdditionalCostRepeatability {
    pub fn is_once(&self) -> bool {
        matches!(self, Self::Once)
    }

    pub fn is_repeatable(self) -> bool {
        matches!(self, Self::Repeatable)
    }
}

/// Serde adapter for legacy `repeatable: bool` on `AdditionalCost`.
pub mod additional_cost_repeatability_bool_compat {
    use super::AdditionalCostRepeatability;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        repeatability: &AdditionalCostRepeatability,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_bool(repeatability.is_repeatable())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<AdditionalCostRepeatability, D::Error> {
        let repeatable = bool::deserialize(deserializer)?;
        Ok(if repeatable {
            AdditionalCostRepeatability::Repeatable
        } else {
            AdditionalCostRepeatability::Once
        })
    }
}

/// Whether a filter predicate includes or excludes the ability source object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SourceExclusion {
    /// The source object may satisfy the predicate.
    #[default]
    Include,
    /// The source object is excluded ("another Aura/Equipment" semantics).
    Exclude,
}

impl SourceExclusion {
    pub fn is_include(&self) -> bool {
        matches!(self, Self::Include)
    }

    pub fn is_exclude(self) -> bool {
        matches!(self, Self::Exclude)
    }
}

/// Serde adapter for legacy `exclude_source: bool` on `FilterProp::HasAttachment`.
pub mod source_exclusion_bool_compat {
    use super::SourceExclusion;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        exclusion: &SourceExclusion,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_bool(exclusion.is_exclude())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<SourceExclusion, D::Error> {
        let exclude_source = bool::deserialize(deserializer)?;
        Ok(if exclude_source {
            SourceExclusion::Exclude
        } else {
            SourceExclusion::Include
        })
    }
}

/// CR 608.2c: Producer-action provenance for a tracked-set member — the keyword
/// action (or one-shot zone change) that made the object part of a "this way"
/// set. This is the IDENTITY of a "<verb>ed this way" relationship: it is the
/// ACTION performed on the member, NOT the zone the member finally landed in.
///
/// Binding "this way" consumers to the action rather than the destination zone
/// is required for two reasons (issue #2932):
///
///   1. CR 608.2c ordering — multiple distinct producer actions share the same
///      destination. Sacrifice (CR 701.21a), destroy (CR 701.8a), mill
///      (CR 701.17a), and discard (CR 701.9a) all land in the graveyard, so a
///      chain that mills AND sacrifices before a later "sacrificed this way"
///      consumer cannot be disambiguated by zone alone.
///   2. CR 614.1 / CR 614.6 replacement redirection — a replacement effect can
///      change a member's destination. With a Rest-in-Peace-style replacement a
///      sacrificed or discarded object lands in Exile, but it was still
///      *sacrificed / discarded this way*. The cause is the action, so it
///      survives the redirect; a zone binding would miss it.
///
/// Each variant cites the keyword action that produces it. `Returned` /
/// `Bounced` are plain zone changes governed by CR 400.7 (no dedicated keyword
/// action), distinguished by destination (battlefield vs. hand).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThisWayCause {
    /// CR 701.13a: the member was exiled this way.
    Exiled,
    /// CR 701.21a: the member was sacrificed this way (cause survives a
    /// replacement that redirects the sacrifice to another zone — CR 614.6).
    Sacrificed,
    /// CR 701.8a: the member was destroyed this way.
    Destroyed,
    /// CR 701.17a: the member was milled this way.
    Milled,
    /// CR 701.9a: the member was discarded this way (cause survives a
    /// replacement that redirects the discard to another zone — CR 614.6).
    Discarded,
    /// CR 608.2c + CR 400.7: the member was returned (put onto the battlefield)
    /// this way by a one-shot put-onto-battlefield instruction.
    Returned,
    /// CR 608.2c + CR 400.7: the member was bounced (returned to its owner's
    /// hand) this way by a mass-bounce instruction.
    Bounced,
}

/// CR 113.3b / CR 113.3c: Which stack ability kinds a `StackAbility` filter accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum StackAbilityKind {
    Activated,
    Triggered,
}

/// Typed target filter replacing all Forge filter strings and TargetSpec.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TargetFilter {
    None,
    Any,
    Player,
    Controller,
    /// CR 102.2 + CR 102.3 + CR 601.2c: A player reference to an opponent of the
    /// ability's controller, used as the announcing player (`target_chooser`) for
    /// a slot whose Oracle text reads "of an opponent's choice". The casting
    /// controller chooses and records that opponent before target selection in
    /// multiplayer; the sole opponent is used in two-player games. This is a
    /// *player-reference* role only — it is never used as an object-population
    /// filter (an opponent-controlled object is expressed as
    /// `Typed(.., controller: Some(ControllerRef::Opponent))`).
    Opponent,
    SelfRef,
    /// CR 201.5a: The specific object that GRANTED the ability this filter lives
    /// inside — used when a granted (activated/triggered) body refers to the
    /// granting object by its printed name (e.g. an Equipment's granted body
    /// "Exile <equipment-name>" / "Return <equipment-name> to its owner's
    /// hand"). Distinct from `SelfRef`, which is the object the ability is ON
    /// (the host creature). Emitted at parse time by the quote masker in
    /// `normalize_card_name_refs`; always concretized to `SpecificObject { id }`
    /// (the live granting-object id) at grant-clone time (`game/layers.rs`).
    /// If it ever reaches runtime unconcretized it degrades to the ability
    /// source (host) — fail-safe, never worse than the pre-fix behavior.
    ///
    /// ZONE-MOVE SCOPING (CR 201.5a second sentence + CR 400.7): the grant-time
    /// concretization snapshots the granter's current battlefield id. CR 201.5a's
    /// second sentence (a source moved to a new public zone → the name refers to
    /// the new-zone object) is not modeled; no current card moves its granter and
    /// then re-references it within one resolution (cost-exile/sacrifice cards
    /// consume the granter before the effect; boomerangs return themselves last).
    GrantingObject,
    /// CR 702.95b: Resolves to the source object and the creature it is paired
    /// with. If the source is not paired, this matches no objects.
    SourceOrPaired,
    Typed(TypedFilter),
    Not {
        filter: Box<TargetFilter>,
    },
    Or {
        filters: Vec<TargetFilter>,
    },
    And {
        filters: Vec<TargetFilter>,
    },
    /// Matches non-mana activated or triggered abilities on the stack.
    /// Used by "counter target activated or triggered ability" effects.
    /// CR 113.7a: once activated or triggered, an ability exists on the stack
    /// independently of its source — `tag` matches by keyword-origin marker
    /// (e.g. `AbilityTag::Backup` for "becomes the target of a backup ability").
    /// `kind` narrows to one ability class when the Oracle text does (Consign to
    /// Memory's "triggered ability" leg); `None` accepts both.
    StackAbility {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        controller: Option<ControllerRef>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tag: Option<AbilityTag>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<StackAbilityKind>,
    },
    /// Matches spells on the stack (not activated/triggered abilities).
    /// CR 115.1a: Used by "becomes the target of a spell" triggers to filter source type.
    StackSpell,
    /// Matches a specific permanent by ObjectId.
    /// Used for duration-based statics that target a specific object
    /// (e.g., "that permanent loses all abilities for as long as ~").
    SpecificObject {
        id: ObjectId,
    },
    /// CR 113.10 + CR 702.16j: Matches a specific player by `PlayerId`.
    /// Used for duration-based statics scoped to a single player
    /// (e.g., Teferi's-Protection-style "you gain protection from everything").
    /// Registered at resolution time by `register_transient_effect` when the
    /// static's `affected` resolves to the ability's controller.
    SpecificPlayer {
        id: PlayerId,
    },
    /// CR 607.2d / CR 607.2m (by analogy): matches every player whose durable
    /// per-player choice (`Player.chosen_attributes`) records the anchor `label`
    /// ("each player who last chose green anchor …", Two Streams Facility).
    /// Evaluated against `game::players::player_last_chose_label`. The player-axis
    /// mirror of the object-scoped anchor gate `StaticCondition::ChosenLabelIs`;
    /// the split (player vs object subject) mirrors the engine's existing
    /// `SpecificPlayer` vs `SpecificObject` separation.
    PlayerWhoChoseLabel {
        label: String,
    },
    /// CR 102.1 + CR 103.1: living player seated immediately to controller's
    /// left/right; clockwise turn order, right = previous seat; resolved
    /// against `state.seat_order`. The recipient is computed at the resolver
    /// (`game::players::neighbor`), never selected as an interactive target
    /// slot.
    Neighbor {
        direction: SeatDirection,
    },
    /// CR 115.10 + CR 608.2c: The current player being affected by an
    /// "each player/opponent" instruction during resolution. This is not the
    /// ability controller; "you" and "your" still refer to `controller`.
    ScopedPlayer,
    /// Matches the permanent that the trigger source (Equipment/Aura) is attached to.
    /// Used for "equipped creature" / "enchanted creature" trigger subjects.
    AttachedTo,
    /// Resolves to the most recently created token(s) from Effect::Token.
    /// Used for "create X and [verb] it" patterns (e.g. "create a token and suspect it").
    LastCreated,
    /// CR 701.20e + CR 608.2c: Resolves to the most recently looked-at or
    /// revealed card(s) from a `Dig`/`RevealTop` effect in the current
    /// resolution (`state.last_revealed_ids`). Used by anaphoric "it" /
    /// "that card" references after "look at the top card of your library"
    /// (Amareth, the Lustrous) and by `AbilityCondition::ObjectsShareQuality`
    /// subject slots.
    LastRevealed,
    /// CR 400.7j + CR 608.2k: Resolves to the object paid as a cost for the
    /// resolving spell or ability. Used by effects such as "the exiled card"
    /// after an exile-as-cost clause.
    CostPaidObject,
    /// CR 613.1f + CR 611.2c + CR 400.7: Resolves to the single card most recently
    /// recorded on the FILTER's source object via `ChosenAttribute::Card` (written
    /// by `Effect::RememberCard`). Models "the last chosen card" — Koh, the Face
    /// Stealer's Layer-6 grant source. Live, not snapshotted: re-evaluated each
    /// layer pass, so re-choosing replaces the grant and the chosen card leaving
    /// its zone (CR 400.7 — a new object) drops the grant. The object matcher
    /// reads `chosen_attributes` from the source (not the resolving ability), so
    /// the static grant resolves it against the permanent that HAS the static.
    ChosenCard,
    /// Matches exactly the objects in a tracked set.
    /// CR 603.7: Delayed triggers act on specific objects from the originating effect.
    TrackedSet {
        id: super::identifiers::TrackedSetId,
    },
    /// CR 701.33 + CR 701.18: Intersection of a tracked set with a type filter.
    /// Matches objects that are BOTH members of the tracked set AND satisfy the
    /// inner type filter. Used to route "X cards revealed this way" downstream
    /// sub_abilities — the Dig resolver populates a tracked set with the cards
    /// kept (revealed) by the player's selection, and follow-up effects like
    /// "Put all land cards revealed this way onto the battlefield tapped" use
    /// this variant to restrict their targets to only the revealed subset that
    /// matches the type filter.
    ///
    /// Example: Zimone's Experiment produces `TrackedSetFiltered { id: 0
    /// /* sentinel resolved to the most recent tracked set */, filter:
    /// Typed(Land), caused_by: None }` for the land-routing sub_ability.
    ///
    /// CR 608.2c: `caused_by` binds the consumer to the producer ACTION that
    /// made each member part of the set — NOT its final landing zone. `None`
    /// (the legacy default at every existing construction site) matches any
    /// member of the set — selection sets ("revealed this way", dig anaphors)
    /// carry no producer action, so they stay action-agnostic.
    /// `Some(cause)` restricts the match to members whose recorded producer
    /// action (`GameState::tracked_set_member_causes`) equals `cause`, so a
    /// merged exile→sacrifice chain set can serve both "exiled this way"
    /// (`Some(Exiled)`) and "sacrificed this way" (`Some(Sacrificed)`)
    /// references disjointly — and a sacrifice that a replacement redirects to
    /// Exile (CR 614.6) still counts as `Sacrificed` (issue #2932).
    TrackedSetFiltered {
        id: super::identifiers::TrackedSetId,
        filter: Box<TargetFilter>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caused_by: Option<ThisWayCause>,
    },
    /// CR 607.2a: Cards exiled by a specific source via "exile until ~ leaves" links.
    /// Resolves via relational `state.exile_links` lookup, not intrinsic object properties.
    ExiledBySource,
    /// CR 607.2b: References a specific card exiled by the source, indexed by order.
    /// Used by The Mimeoplasm to distinguish "the first card exiled this way" from
    /// "the second card exiled this way". The index is 0-based and corresponds to
    /// the order in `state.cards_exiled_with_source_this_turn[source_id]`.
    /// ENGINE INVARIANT: The ordering is guaranteed by Vec::push in push_exiled_with_source_this_turn.
    ExiledCardByIndex {
        index: u32,
    },
    /// CR 603.7c: Resolves to the controller of the spell/ability that triggered this.
    TriggeringSpellController,
    /// CR 603.7c: Resolves to the owner of the spell/ability that triggered this.
    TriggeringSpellOwner,
    /// CR 603.7c: Resolves to the player involved in the triggering event.
    TriggeringPlayer,
    /// CR 603.7c: Resolves to the source object of the triggering event.
    TriggeringSource,
    /// CR 603.2 + CR 120.1: Resolves to the object that *received* the damage
    /// referenced by the current trigger event — the recipient counterpart to
    /// [`TargetFilter::TriggeringSource`] and the `TargetFilter`-side analogue of
    /// [`ObjectScope::EventTarget`]. This binds "that creature" / "that
    /// permanent" in an intervening-`if` (CR 603.4) to the *specific* damaged
    /// object carried by the `DamageDealt` event, not a generic type filter, so
    /// "if that creature was dealt excess damage this turn" (Maarika, Brutal
    /// Gladiator) checks only the creature this trigger's damage went to.
    /// Resolved via `extract_target_object_from_event` against
    /// `state.current_trigger_event`; matches no object outside a trigger.
    /// DealDamage has a narrow CR 115.10a / CR 120.3 exception for "that
    /// permanent or player": its resolver reads the raw `DamageDealt.target`,
    /// which may be `TargetRef::Player`. Generic filter, targeting, and
    /// quantity paths remain object-only.
    EventTarget,
    /// CR 603.7c + CR 109.4 + CR 110.2: Resolves to the *controller* of the
    /// triggering event's source object — the player-level counterpart of
    /// `TriggeringSource`, mirroring how `TriggeringSpellController` is the
    /// controller of `StackSpell`. Used by "the attacking player" / "its
    /// controller" anaphors on `DamageReceived` triggers where the wanted
    /// player is the controller of the creature that dealt combat damage, not
    /// the damaged player (`TriggeringPlayer`). Powers Contested Game Ball
    /// ("Whenever you're dealt combat damage, the attacking player gains
    /// control of this artifact and untaps it.").
    TriggeringSourceController,
    /// Resolves to the same target(s) as the parent ability.
    /// Used for anaphoric "it"/"that creature"/"that player" in compound effects
    /// (e.g., "tap target creature and put a stun counter on it").
    /// At resolution time, the sub_ability chain inherits parent targets automatically.
    ParentTarget,
    /// CR 608.2c: Resolves to a specific target slot from the parent ability.
    /// Used when later English anaphors distinguish multiple prior targets by
    /// role ("the artifact" vs. "the artifact card") and live-state filtering
    /// would be ambiguous after earlier instructions mutate zones.
    ParentTargetSlot {
        index: usize,
    },
    /// CR 608.2c: Resolves to the controller of the parent ability's target object.
    /// Used for "its controller" in compound effects (e.g., "counter target spell. Its controller
    /// loses 2 life."). At resolution time, looks up the controller of the first parent target.
    ParentTargetController,
    /// CR 108.3 + CR 608.2c: Resolves to the *owner* of the parent ability's target
    /// object. Used for "its owner" anaphoric references where the acting player is the
    /// owner of a previously-mentioned permanent — e.g., Enslave's "enchanted creature
    /// deals 1 damage to its owner" or Bomb Squad's "that creature deals 4 damage to
    /// its owner". Resolution mirrors `ParentTargetController` (parent target slot →
    /// trigger-event source → AttachedTo host on source for Aura phase triggers), but
    /// returns the resolved object's `owner` rather than `controller` per CR 108.3.
    ///
    /// Distinct from `Owner` (which always reads the source object's owner) and
    /// `ParentTargetController` (which returns the controller per CR 109.4).
    ParentTargetOwner,
    /// CR 607.2d + CR 608.2c: Resolves to the player chosen for the source by
    /// a linked persisted choice ("the chosen player"). This is not a target
    /// slot and is distinct from `ControllerRef::ChosenPlayer`, which is
    /// resolution-scoped to the current ability chain.
    SourceChosenPlayer,
    /// CR 109.5 + CR 608.2c: Resolves to the ability's *original* controller — the
    /// player who put the spell or ability on the stack — even when a surrounding
    /// `player_scope` iteration has rebound `ResolvedAbility::controller` to a
    /// different player for the current per-player iteration.
    ///
    /// At resolution time, returns `ability.original_controller.unwrap_or(ability.controller)`.
    /// This mirrors the quantity-layer behavior in `resolve_quantity_with_targets`,
    /// where "you" / "your" quantities always read the original controller.
    ///
    /// Used by parser-level distribution of compound subjects like
    /// "you and that player each Y" — the first half ("you") MUST resolve to the
    /// printed ability controller, not the iterated voter, even when the parent
    /// ability has `player_scope: PlayerFilter::VotedFor` (Master of Ceremonies
    /// pattern, CR 800.4g).
    ///
    /// Distinct from `Controller` (which resolves to `ability.controller` and
    /// is rebound per-iteration by the player_scope driver in
    /// `resolve_ability_chain`). Distinct from `ParentTargetController`
    /// (parent's target's controller) and `PostReplacementSourceController`
    /// (replacement-context source's controller).
    OriginalController,
    /// CR 613.1f + CR 608.2c: Resolves to the ability's *original*, pre-rebind
    /// source identity — the object that put this ability on the stack — even
    /// inside a `ChangeZone { forward_result: true }` sub-ability chain where
    /// the runtime has rebound `ResolvedAbility::source_id` to the just-moved
    /// object.
    ///
    /// Motivation (reanimator-Aura class: Animate Dead / Dance of the Dead):
    /// the ETB effect returns the enchanted creature card to the battlefield
    /// via `ChangeZone { forward_result: true }`, which rebinds `source_id` to
    /// the reanimated creature so trailing `SelfRef`/`ParentTarget` anaphora
    /// (the attach, the leaves-battlefield sacrifice) bind to that creature.
    /// But the same effect ALSO rewrites the Aura's OWN enchant restriction
    /// ("it loses \"enchant creature card in a graveyard\" and gains \"enchant
    /// creature put onto the battlefield with this Aura\""). That keyword swap
    /// must reference the Aura itself (the original source), not the rebound
    /// creature — CR 613.1f governs this layer-6 keyword swap of the Aura's own
    /// enchant ability (defined per CR 303.4a), and CR 608.2c requires reading
    /// the whole sentence to bind each anaphor to its true referent at resolution.
    ///
    /// Never persisted as a `ResolvedAbility` field: it is concretized eagerly,
    /// in place, to `SpecificObject { id }` in the `forward_result` block of
    /// `effects/mod.rs`, at the one point where the pre-rebind `source_id` and
    /// the about-to-be-mutated sub-ability clone coexist.
    ///
    /// Distinct from `SelfRef` (rebound to the moved object under
    /// `forward_result`) and from `OriginalController` (a player reference).
    OriginalSource,
    /// CR 615.5 + CR 609.7: Resolves to the controller of the *prevented event's*
    /// damage source. Used by prevention follow-up sentences such as "the source's
    /// controller draws cards equal to the damage prevented this way" (Swans of
    /// Bryn Argoll) and "deals damage to that source's controller" (Deflecting
    /// Palm class). At resolution time, looks up `state.post_replacement_event_source`
    /// and returns its controller.
    ///
    /// Distinct from `ParentTargetController` (which resolves via the parent
    /// ability's target slot) and `TriggeringSpellController` (which resolves
    /// via `state.current_trigger_event`, which is `None` during post-replacement
    /// resolution). Architectural twin of the quantity-side `last_effect_count`
    /// fallback at `replacement.rs:317` — both stash event context that lives
    /// outside the trigger window. The parser never emits this variant directly;
    /// the prevention follow-up call site rewrites `ParentTargetController`
    /// → `PostReplacementSourceController` via `each_target_filter_mut` after
    /// `parse_effect_chain` returns, so the surface phrase "the source's
    /// controller" can stay consolidated in `parse_target` for non-prevention
    /// callers.
    PostReplacementSourceController,
    /// CR 615.5: Resolves to the player or permanent that was the target of the
    /// prevented damage event. Used by prevention follow-up sentences such as
    /// "that player exiles that many cards" where the affected player is the
    /// damage recipient, not the replacement source or damage source.
    PostReplacementDamageTarget,
    /// CR 108.3 + CR 400.3 + CR 615.5: Resolves to the *owner* of the prevented
    /// event's damage recipient. Event-driven analog of `ParentTargetOwner`
    /// (owner-projection), exactly as `PostReplacementDamageTarget` is the
    /// event-driven analog of `ParentTarget`. Used by "prevent that damage and
    /// that creature's owner shuffles it into their library" (Weeping Angel),
    /// where "their library" is the recipient's OWNER's library (CR 108.3),
    /// routed there by CR 400.3 — NOT the shield controller's library.
    ///
    /// Resolves the OWNER (not controller) of the object in
    /// `state.post_replacement_event_target` — mirrors
    /// `PostReplacementSourceController`'s player-projection mechanism, NOT
    /// `PostReplacementDamageTarget`'s raw object return. CR 109.4 (controller)
    /// is the rule this variant is deliberately DISTINCT from; the same
    /// owner-vs-controller split the sibling `ParentTargetOwner` cites.
    PostReplacementDamageTargetOwner,
    /// CR 508.5 / CR 508.5a: Resolves to the defending player for the source
    /// attacking creature, looked up per attacker through
    /// `combat::defending_player_for_attacker`.
    DefendingPlayer,
    /// Matches objects whose name equals the source's ChosenAttribute::CardName.
    /// Used for "card with the chosen name" patterns.
    HasChosenName,
    /// CR 609.7a: Matches the object stored as the source's chosen damage source.
    /// Resolution-time prevention effects should resolve this to `SpecificObject`
    /// when the shield is created so global shields do not depend on a live source.
    /// CR 609.7 + CR 609.7b: `filter` optionally constrains which sources are LEGAL
    /// to choose — e.g. "a blue source of your choice" (Circle/Rune of Protection)
    /// restricts the choice to blue sources and is rechecked against the chosen
    /// object's live properties when damage would be dealt; `None` is the bare "a
    /// source of your choice" / "that source" form (any source is a legal choice).
    ChosenDamageSource {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filter: Option<Box<TargetFilter>>,
    },
    /// Matches objects with a specific hardcoded name.
    /// Used for "card named [literal]" patterns.
    Named {
        name: String,
    },
    /// CR 400.3: Resolves to the owner of the object referenced by `source_id`.
    /// Used for "its owner's library/hand/graveyard" phrasings where the acting
    /// player must be the card's owner, not its current controller (e.g.,
    /// Nexus of Fate's shuffle-back replacement when the card is under opposing
    /// control via Mind Control).
    Owner,
    /// CR 118.12a: Every player in the game (controller + opponents), polled in
    /// APNAP order. The unless-payer population for "[Effect] unless any player
    /// pays ..." clauses (Cleansing, Rhystic cycle, Soul Strings). Resolution is
    /// a sequential poll — the first player to pay prevents the effect — so this
    /// variant is only valid as an `UnlessPayModifier.payer`; it is never used
    /// as a target or affected filter.
    AllPlayers,
}

/// CR 701.26a / CR 701.26b: Selection scope for `Effect::SetTapState`. Picks
/// whether the tap/untap targets a single chosen/source permanent (the legacy
/// `Tap` / `Untap`) or every permanent matching the filter (the legacy
/// `TapAll` / `UntapAll`). Parameterizes the structural "single vs mass"
/// axis instead of proliferating sibling effect variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum EffectScope {
    /// One permanent — `target` is a selectable target filter (legacy
    /// `Effect::Tap` / `Effect::Untap`). `target_filter()` exposes it.
    Single,
    /// Every permanent matching the filter — `target` is a non-targeting
    /// population filter (legacy `Effect::TapAll` / `Effect::UntapAll`).
    All,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReassembleControlMode {
    #[default]
    KeepController,
    GainControl,
}

/// CR 701.26a (tap) / CR 701.26b (untap): Direction of an `Effect::SetTapState`.
/// Parameterizes the tap/untap axis so a single effect variant covers both
/// keyword actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TapStateChange {
    /// CR 701.26a: Turn the permanent sideways (tap it).
    Tap,
    /// CR 701.26b: Rotate the permanent upright (untap it).
    Untap,
}

/// CR 709.5f-g: Operation an [`Effect::SetRoomDoorLock`] performs on a door
/// (half) of a Room permanent. Typed (not a bool) so serialized engine state
/// and the door-choice prompt say which direction is being applied, and so the
/// "lock or unlock" disjunction is a first-class third operation rather than an
/// inexpressible boolean combination. Parameterizes the lock/unlock axis — both
/// live within CR rule section 709.5 — into one effect variant instead of two
/// sibling effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DoorLockOp {
    /// CR 709.5f: choose a locked half and give it the unlocked designation.
    Unlock,
    /// CR 709.5g: choose an unlocked half and remove its unlocked designation.
    Lock,
    /// CR 709.5f + CR 709.5g: "Lock or unlock a door" — the player chooses both
    /// the operation and the eligible half at resolution (Keys to the House,
    /// Marina Vendrell).
    LockOrUnlock,
}

/// CR 102 + CR 119 + CR 402: Player axis for player-scoped quantity references.
///
/// Parameterizes the player whose hand size, life total, or other per-player
/// scalar is being read. Replaces sibling enum variants like
/// `LifeTotal` / `TargetLifeTotal` / `OpponentLifeTotal` and
/// `HandSize` / `OpponentHandSize` with a single parameterized form.
///
/// Categorical-boundary check (per the "Parameterize, don't proliferate"
/// principle): every variant is a player-relativity choice within a single
/// CR section. Aggregate scopes (`Opponent`, `AllPlayers`) compose
/// `AggregateFunction` to express max/min/sum across a population — they do
/// not introduce a new abstraction layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PlayerScope {
    /// CR 109.5 / CR 113.6: The controller of the source ability or effect.
    Controller,
    /// CR 115.10 + CR 608.2c: The current player being affected by an
    /// "each player/opponent" instruction during resolution. This keeps
    /// "their" quantities separate from "you" quantities.
    ScopedPlayer,
    /// CR 109.4 + CR 113.6 + CR 115.1: The first player target of the
    /// resolving ability (read from `ability.targets`).
    Target,
    /// CR 102.2 + CR 102.3: All opponents of the controller, aggregated by
    /// `aggregate`. Existing `OpponentLifeTotal` / `OpponentHandSize`
    /// semantics correspond to `aggregate = Max`.
    Opponent { aggregate: AggregateFunction },
    /// CR 102.1: All players in the game (controller + opponents),
    /// aggregated by `aggregate`. Reserved for future cards that read
    /// "the highest life total among players" or similar cross-player
    /// extrema that include the controller.
    AllPlayers {
        aggregate: AggregateFunction,
        /// CR 102.1 + CR 608.2c: when `Some`, the named player is excluded
        /// from the aggregated population ("each OTHER player"). The
        /// excluded player is itself a `PlayerScope` — the type composes
        /// with itself, generalizing "each other player" for any anchor.
        /// `None` = all players.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exclude: Option<Box<PlayerScope>>,
    },
    /// CR 303.4m + CR 613.4c: The controller of the object currently
    /// receiving a layer effect. Used for Aura/Equipment statics such as
    /// "enchanted creature gets +1/+1 for each card in its controller's
    /// hand", where "its" refers to the enchanted creature, not the Aura.
    RecipientController,
    /// CR 508.5 / CR 508.5a: The defending player for the source attacking
    /// creature, resolved per attacker through
    /// `combat::defending_player_for_attacker`. Used by attack-trigger
    /// intervening-if quantities such as "no opponent has more life than that
    /// player."
    DefendingPlayer,
    /// CR 109.4 + CR 608.2c: The controller of the first object target of
    /// the resolving ability ("that opponent" anaphoring the controller of
    /// a bounced/destroyed creature). The player-scalar-axis analogue of
    /// `ControllerRef::ParentTargetController`. Resolved via
    /// `ability_utils::parent_target_controller`.
    ParentObjectTargetController,
    /// CR 613.1 + CR 109.4: The player PERSISTED on the source via
    /// `ChosenAttribute::Player` (an "as ~ enters the battlefield, choose a
    /// player" replacement). The player-scalar-axis analogue of
    /// `ControllerRef::SourceChosenPlayer`, read continuously from the source's
    /// `chosen_attributes` so a CDA P/T can track e.g. the chosen player's hand
    /// or graveyard size (Entropic Specter, Sewer Nemesis).
    SourceChosenPlayer,
    /// CR 513.1 + CR 611.2a + CR 603.7b: turn-AGNOSTIC deadline — the referenced
    /// step's NEXT occurrence regardless of whose turn ("the next end step", e.g.
    /// Niko, Light of Hope), as opposed to `Controller` ("your next end step",
    /// Rocco/Street Chef). Mirrors the delayed-trigger split `AtNextPhase`
    /// (agnostic) vs `AtNextPhaseForPlayer` (scoped). DURATION-TIMING-ONLY:
    /// constructed solely in the "the next end step" parser arm inside
    /// `Duration::UntilNextStepOf` — never from a value/quantity/player-selection
    /// position.
    AnyTurn,
}

/// Scope selector for object-axis quantities (Round Π-5). Picks WHICH object
/// to read from when a `QuantityRef` (and future per-object conditions) is
/// per-object. Mirrors `PlayerScope` for the player axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ObjectScope {
    /// CR 113.7 + CR 113.7a: The source object of the resolving ability —
    /// "this creature", "~", "it" (when "it" anaphors back to the source).
    /// CR 113.7 defines the source as the object that generated the ability;
    /// CR 113.7a keeps the ability (and thus its source reference) valid on
    /// the stack even after the source leaves its expected zone, via LKI.
    Source,
    /// CR 115.1: The first object target of the resolving ability — "that
    /// creature", "the creature", "it" (when "it" anaphors back to a target).
    Target,
    /// CR 613.4c + CR 115.10: The object currently receiving an effect.
    /// In layer evaluation this is the per-object recipient. Outside layers,
    /// it resolves to the first object target when present, then to the source.
    /// Used for recipient-relative "its colors" boosts such as Blessing of
    /// the Nephilim and Civic Saber.
    Recipient,
    /// CR 603.2: The object referenced by the current trigger event.
    EventSource,
    /// CR 608.2k: The specific untargeted object previously referred to by
    /// this ability's cost OR trigger condition. Resolved (first match wins)
    /// via `ResolvedAbility.cost_paid_object` → trigger-event source →
    /// `effect_context_object`. Subsumes the former `EventContextSource*`
    /// trio (CR 117.1 + CR 400.7j cost referent and CR 603.2 trigger
    /// referent are the two enumerated members of CR 608.2k's single clause).
    CostPaidObject,
    /// CR 608.2k: A deferred anaphoric **pronoun** ("it" / "its") whose object
    /// referent is bound at parse time. The parser rebinds this to a concrete
    /// scope wherever the enclosing clause establishes the antecedent —
    /// `Source` when the clause subject is the ability source, `Target` when
    /// the recipient is "itself", `EventSource` when the subject is the
    /// triggering object. The rebind ([`rebind_anaphoric_object_scope`] in
    /// `parser/oracle_effect/mod.rs`) covers every per-object characteristic
    /// (power, toughness, mana value, …) — not just power — because the
    /// pronoun refers to one object and the rebind only retargets *which*
    /// object, never *which* characteristic. When no clause subject applies
    /// (triggered-ability anaphora) `Anaphoric` survives to runtime, where it
    /// resolves identically to a demonstrative (effect-context referent first;
    /// see [`ObjectScope::Demonstrative`]). General rules-correct runtime
    /// resolution of triggered-ability anaphora (e.g. "its mana value" after a
    /// reveal — see issue #511) is separate per-card parser work.
    Anaphoric,
    /// CR 608.2c: A **demonstrative / definite** possessive back-reference
    /// ("that creature's", "that card's", "that spell's", "the creature's")
    /// whose antecedent is a full noun phrase naming an object introduced by an
    /// earlier instruction in the same ability — *not* the grammatical subject
    /// of the clause it appears in. Unlike [`ObjectScope::Anaphoric`] (the
    /// pronoun "its"), the subject-injection rewrite MUST NOT rebind this — its
    /// antecedent is fixed by the Oracle text. Steadfast Armasaur ("its
    /// toughness", `Anaphoric` → rebound to `Source`) and Creature Bond ("that
    /// creature's toughness", `Demonstrative` → never rebound) parse to the
    /// same `QuantityRef` property and differ only by this scope: collapsing
    /// them caused the LKI-toughness bug. At runtime `Demonstrative` resolves
    /// identically to `Anaphoric` — `effect_context_object` (CR 608.2c
    /// instruction-order referent) first, then the trigger source (CR 608.2k),
    /// then the cost-paid object. This is the Yuriko / Dark Confidant / Mana
    /// Drain class (issue #511): a reveal/counter/reanimate earlier in the same
    /// ability binds "that <type>'s" to the referenced object.
    Demonstrative,
    /// CR 701.47c + CR 608.2c: The Army creature chosen by the current amass
    /// instruction. "The Army you amassed" and "the amassed Army" refer to
    /// that chosen creature even if it didn't receive counters. This is
    /// resolution-local state carried by `ResolvedAbility.amassed_army_object`,
    /// not the generic demonstrative/effect-context slot.
    AmassedArmy,
    /// CR 603.2 + CR 120.1: The object that **received** the damage referenced
    /// by the current trigger event — the recipient counterpart to
    /// [`ObjectScope::EventSource`]. This is "that creature" in "deals
    /// noncombat damage to a creature equal to that creature's toughness":
    /// the antecedent is the damaged object carried by the `DamageDealt` event,
    /// not the ability source or a target. Resolved at both trigger detection
    /// and resolution (CR 603.4 intervening-if) via
    /// `extract_target_object_from_event`.
    EventTarget,
    /// CR 608.2c + CR 701.20b + CR 108.3 + CR 202.3: In an exactly-two-target
    /// symmetric reveal ("two target players each reveal the top card of their
    /// library"), the revealed card belonging to the OTHER of the two revealers —
    /// the referent of "the card revealed by the other player" (Parker Luck).
    /// Distinct from [`ObjectScope::Demonstrative`]/[`ObjectScope::Anaphoric`],
    /// which resolve to `effect_context_object` = the reader's OWN revealed card.
    /// Resolved by exclusion: it is the single `state.last_revealed_ids` entry
    /// whose id is NOT the per-iteration `effect_context_object` (the reader's own
    /// card, bound owner-keyed per CR 108.3). CR 701.20b keeps both cards live in
    /// their libraries when the cross-loss reads them; CR 202.3 makes the mana
    /// value zone-independent, so the read is stable even after a card is put to
    /// hand. Fail-closed to a null read (→ 0) when no "other" entry exists (empty
    /// library or an illegal target on resolution, CR 608.2b).
    OtherRevealedCard,
}

/// Source set for counting distinct card types.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CardTypeSetSource {
    /// CR 109.2a + CR 400.1: Cards in a specific zone, scoped by player.
    Zone { zone: ZoneRef, scope: CountScope },
    /// CR 607.2a + CR 406.6: Cards exiled by the source's linked ability.
    ExiledBySource,
    /// CR 109.2: Objects matching a battlefield-style filter.
    Objects { filter: TargetFilter },
    /// CR 608.2c + CR 205.2a: distinct card types among the current chain
    /// tracked set, optionally restricted to members produced by `caused_by`.
    /// Mirrors `QuantityRef::FilteredTrackedSetSize { caused_by }`: a merged
    /// Draw->Discard set is disambiguated by CAUSE (drawn members are unstamped),
    /// so Some(Discarded) counts only discarded members. None counts all.
    TrackedSet {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caused_by: Option<ThisWayCause>,
    },
}

/// CR 205.3: Which subtypes are excluded when counting distinct subtypes.
///
/// A typed qualifier (not a `bool`) so the exclusion axis stays composable and
/// extensible — `CreatureTypes` covers Subgoyf ("subtypes other than creature
/// types"); future readings ("subtypes other than land types") add a variant
/// rather than a second boolean. `None` counts every subtype value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "type")]
pub enum SubtypeExclusion {
    /// Count every distinct subtype value with no exclusion.
    #[default]
    None,
    /// CR 205.3m: Exclude subtypes that are creature types (read from
    /// `GameState::all_creature_types`).
    CreatureTypes,
}

/// CR 601.2h: Which cast object a mana-spent quantity reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CastManaObjectScope {
    /// The ability's source object, or the entering object in ETB replacement context.
    SelfObject,
    /// The spell object referenced by the current trigger event.
    TriggeringSpell,
    /// CR 115.1 + CR 601.2h: The first object target of the resolving ability —
    /// "it"/"that spell" when the condition gates on the targeted spell
    /// (Nix: "Counter target spell if no mana was spent to cast it").
    AbilityTarget,
}

/// CR 106.3 + CR 601.2h: What to measure about mana spent to cast a spell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CastManaSpentMetric {
    /// Total amount of mana spent.
    Total,
    /// Number of distinct colors of mana spent.
    DistinctColors,
    /// Amount of mana whose source matched the filter at payment time.
    FromSource { source_filter: TargetFilter },
}

/// A dynamic game quantity — a runtime lookup into the game state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum QuantityRef {
    /// CR 402: Number of cards in `player`'s hand. `PlayerScope::Controller`
    /// is the default reading; `Target`, `Opponent { .. }`, and `AllPlayers`
    /// cover targeted-player and cross-player aggregate variants.
    HandSize { player: PlayerScope },
    /// CR 119: `player`'s current life total. See `HandSize` for player-axis
    /// semantics.
    LifeTotal { player: PlayerScope },
    /// CR 404: Number of cards in the scoped player's graveyard. `PlayerScope`
    /// follows the same player-axis semantics as `HandSize` / `LifeTotal`:
    /// `Controller` is the default ("your graveyard"); `Opponent { aggregate }`
    /// covers "an opponent['s] graveyard" thresholds (e.g. Merfolk Windrobber,
    /// See Double — "if an opponent has eight or more cards in their graveyard").
    GraveyardSize { player: PlayerScope },
    /// Controller's life total minus the format's starting life total.
    /// Used for "N or more life more than your starting life total" conditions.
    LifeAboveStarting,
    /// CR 103.4: The format's starting life total (20 for Standard, 40 for Commander, etc.).
    StartingLifeTotal,
    /// CR 701.57a: The mana-value limit `N` of the discover that fired the
    /// current "whenever you discover" trigger — read from
    /// `GameState::last_discover_value`. Curator of Sun's Creation: "discover
    /// again for the same value". Resolves to 0 outside a discover-trigger
    /// context (fail-safe; no card references it elsewhere).
    TriggeringDiscoverValue,
    /// Count of objects on the battlefield matching a filter.
    /// Used for "for each creature you control" and similar patterns.
    ObjectCount { filter: TargetFilter },
    /// CR 201.2 + CR 603.4: Count of objects matching a filter, deduplicated
    /// by the listed shared qualities. `qualities = [Name]` is the canonical
    /// "seven or more lands with different names" shape (Field of the Dead);
    /// `qualities = [ManaValue]` covers "different mana values"; multiple
    /// qualities form a tuple key (objects whose tuple values all coincide
    /// count as one).
    ///
    /// Lifts the legacy `ObjectCountDistinctNames` leaf to the same
    /// `Vec<SharedQuality>` axis already used by
    /// `SearchSelectionConstraint::DistinctQualities` (Batch 1) so the
    /// count-expression and constraint sides share one quality vocabulary.
    ///
    /// Backward compat: deserializes from the legacy tag
    /// `ObjectCountDistinctNames` (single `filter` field) via
    /// `#[serde(alias)]`, defaulting `qualities` to `vec![SharedQuality::Name]`.
    #[serde(alias = "ObjectCountDistinctNames")]
    ObjectCountDistinct {
        filter: TargetFilter,
        #[serde(default = "default_distinct_names")]
        qualities: Vec<SharedQuality>,
    },
    /// CR 109.3 + CR 205.3m: Count matching objects grouped by a shared object
    /// characteristic, including creature types, then aggregate the group
    /// sizes. Covers "the greatest number of creatures you control that have a
    /// creature type in common" without encoding the shared-quality clause as a
    /// target filter.
    ObjectCountBySharedQuality {
        filter: TargetFilter,
        quality: SharedQuality,
        aggregate: AggregateFunction,
    },
    /// Count of players matching a player-level filter.
    /// Used for "for each opponent who lost life this turn" and similar patterns.
    PlayerCount { filter: PlayerFilter },
    /// Count of counters of a given type on the source object.
    /// Used for "for each [counter type] counter on ~" patterns.
    ///
    /// For counters on a *player* (experience, poison, rad, ticket), use
    /// [`QuantityRef::PlayerCounter`] instead.
    /// CR 122.1: Count of counters on an object, parameterized by scope and
    /// type filter (Round Π-5). Replaces the 4-variant cluster
    /// `CountersOnSelf` / `CountersOnTarget` / `AnyCountersOnSelf` /
    /// `AnyCountersOnTarget`.
    ///
    /// - `scope = Source`: counts on the source permanent ("counter on ~").
    /// - `scope = Target`: counts on the first object target ("counter on
    ///   that creature").
    /// - `counter_type = Some(ct)`: only counters of type `ct`.
    /// - `counter_type = None`: total counters of any type (e.g., Gemstone
    ///   Mine "When there are no counters on ~"; Nils, Discipline Enforcer
    ///   "the number of counters on that creature" per Scryfall ruling).
    ///
    /// For counters on a *player* (experience, poison, rad, ticket), use
    /// [`QuantityRef::PlayerCounter`] instead.
    CountersOn {
        scope: ObjectScope,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        counter_type: Option<CounterType>,
    },
    /// CR 122.1: Total counters across all objects matching a filter.
    /// Used for phrases like "the number of +1/+1 counters on lands you control"
    /// (`counter_type: Some("P1P1")`) and "counters among artifacts and creatures
    /// you control" (`counter_type: None`, sums across every counter type).
    CountersOnObjects {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        counter_type: Option<CounterType>,
        filter: TargetFilter,
    },
    /// CR 122.1: Count of a named player-counter kind on a player (or summed across
    /// scoped players). Distinct from `CountersOnSelf` / `CountersOnTarget` /
    /// `CountersOnObjects`, which count counters on *objects* — player counters
    /// live on `Player`.
    ///
    /// Kind-specific CR references:
    /// - Poison: CR 122.1f + CR 704.5c (ten-or-more SBA).
    /// - Rad:    CR 122.1i + CR 728.
    /// - Experience and Ticket are covered only by the generic CR 122.1.
    ///
    /// Scope is currently limited to `Controller`, `Opponents`, and `All`.
    /// Targeted-player variants ("target opponent has N experience counters")
    /// are not yet represented; extending `CountScope` with a `TargetPlayer`
    /// arm is a future change when a card forces it.
    PlayerCounter {
        kind: PlayerCounterKind,
        scope: CountScope,
    },
    /// CR 122.1f + CR 109.4 + CR 115.1 + CR 608.2c: The `kind` player-counter
    /// total on the controller of the resolving ability's first object target —
    /// "its controller" anaphoring the controller of the targeted object
    /// (Corrupted Resolve: "counter target spell if its controller is
    /// poisoned"; `>= 1` poison == "poisoned" per CR 122.1f). Resolved via
    /// `ability_utils::parent_target_controller`, mirroring the target-relative
    /// `TargetObjectManaValue` sibling of `ObjectManaValue`. Kept distinct from
    /// `PlayerCounter { scope }` — that reads an aggregate over a `CountScope`
    /// player set (controller/opponents/all), whereas this reads one specific
    /// target-derived player, an axis the aggregate `CountScope` cannot express.
    TargetControllerCounter { kind: PlayerCounterKind },
    /// A variable reference (e.g. "X") resolved from spell payment or "that much" from prior effect.
    Variable { name: String },
    /// CR 208.1 + CR 113.6: Current power of an object, scoped via ObjectScope
    /// (Round Π-6). Replaces the `SelfPower` / `TargetPower` sibling pair.
    /// `Source` reads the source object's power (post-layer); `Target` reads
    /// the first object target's power. `CostPaidObject` subsumes the former
    /// `EventContextSourcePower` (CR 608.2k: cost OR trigger-condition
    /// referent).
    Power { scope: ObjectScope },
    /// Digital-only Alchemy (no CR entry): the current `intensity` of an object,
    /// scoped via `ObjectScope`. Reads "X is [card]'s intensity" /
    /// "this spell's intensity" / "equal to its intensity".
    Intensity { scope: ObjectScope },
    /// CR 208.1 + CR 113.6: Current toughness of an object, scoped via
    /// ObjectScope (Round Π-6). Mirrors `Power`. Replaces the `SelfToughness`
    /// variant. `CostPaidObject` subsumes the former
    /// `EventContextSourceToughness` (CR 608.2k).
    Toughness { scope: ObjectScope },
    /// CR 202.3: Mana value of an object, scoped via ObjectScope.
    /// `Source` is the resolving ability's source; `Target` is the first object
    /// target. Used by source/target-relative mana-value filters such as
    /// "with the same mana value as that spell". `CostPaidObject` subsumes the
    /// former `EventContextSourceManaValue` (CR 608.2k).
    ObjectManaValue { scope: ObjectScope },
    /// CR 202.3 + CR 115.1: Mana value of the object chosen for a count-derived
    /// target slot whose legal candidates are `filter` (e.g. "target artifact or
    /// creature you control"). Distinct from `ObjectManaValue { scope: Target }`
    /// (a possessive "that creature's mana value", filterless); this variant owns
    /// its own target slot, surfaced via `quantity_ref_target_slot_spec`.
    TargetObjectManaValue { filter: Box<TargetFilter> },
    /// CR 105.1 + CR 105.2: Number of colors of an object, scoped via
    /// ObjectScope. Counts the object's current W/U/B/R/G color set; colorless
    /// objects return 0. `Recipient` preserves "for 