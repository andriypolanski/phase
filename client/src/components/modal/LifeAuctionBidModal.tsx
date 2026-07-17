import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import { ChoiceOverlay, ConfirmButton } from "./ChoiceOverlay.tsx";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import type { GameAction, WaitingFor } from "../../adapter/types.ts";
import { gameButtonClass } from "../ui/buttonStyles.ts";

type LifeAuctionBid = Extract<WaitingFor, { type: "LifeAuctionBid" }>;

function bidAmountsFromLegalActions(actions: GameAction[]): number[] {
  return actions
    .filter((action): action is Extract<GameAction, { type: "SubmitLifeAuctionBid" }> => {
      return action.type === "SubmitLifeAuctionBid";
    })
    .map((action) => action.data.amount)
    .sort((a, b) => a - b);
}

/**
 * Card-defined open-bid life auction. The engine exposes the standing high bid,
 * high bidder, and legal pass/raise actions; this modal only renders and dispatches.
 */
export function LifeAuctionBidModal({ data }: { data: LifeAuctionBid["data"] }) {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const legalActions = useGameStore((s) => s.legalActions);
  const bidAmounts = useMemo(() => bidAmountsFromLegalActions(legalActions), [legalActions]);
  const minBid = bidAmounts[0] ?? data.high_bid + 1;
  const maxBid = bidAmounts.at(-1) ?? minBid;
  const canPass = legalActions.some((action) => action.type === "PassLifeAuction");
  const canRaise = bidAmounts.length > 0;
  const [amount, setAmount] = useState(minBid);

  useEffect(() => {
    setAmount(minBid);
  }, [minBid, maxBid, data.high_bid, data.player]);

  const handlePass = useCallback(() => {
    if (!canPass) return;
    dispatch({ type: "PassLifeAuction" });
  }, [canPass, dispatch]);

  const handleBid = useCallback(() => {
    if (!canRaise || !bidAmounts.includes(amount)) return;
    dispatch({ type: "SubmitLifeAuctionBid", data: { amount } });
  }, [amount, bidAmounts, canRaise, dispatch]);

  const highBidderLabel = t("lifeAuction.highBidder", { number: data.high_bidder + 1 });

  return (
    <ChoiceOverlay
      title={t("lifeAuction.title")}
      subtitle={t("lifeAuction.subtitle", {
        bid: data.high_bid,
        bidder: highBidderLabel,
      })}
      widthClassName="w-fit max-w-full"
      maxWidthClassName="max-w-xl"
      footer={
        <div className="flex flex-wrap items-center justify-center gap-3">
          {canPass ? (
            <button type="button" onClick={handlePass} className={gameButtonClass({ tone: "neutral", size: "md" })}>
              {t("lifeAuction.pass")}
            </button>
          ) : null}
          {canRaise ? (
            <ConfirmButton onClick={handleBid} disabled={!bidAmounts.includes(amount)}>
              {t("lifeAuction.bid", { amount })}
            </ConfirmButton>
          ) : null}
        </div>
      }
    >
      {canRaise ? (
        <div className="mx-auto mb-2 w-full max-w-md px-2">
          <label className="flex items-center gap-3 text-sm text-gray-200">
            <span className="shrink-0 font-mono text-base text-cyan-300">
              {t("lifeAuction.bidLabel", { amount })}
            </span>
            <input
              type="range"
              min={minBid}
              max={maxBid}
              value={amount}
              onChange={(event) => setAmount(Number(event.target.value))}
              className="h-2 w-full cursor-pointer appearance-none rounded-full bg-gray-700 accent-cyan-500"
              aria-label={t("lifeAuction.bidLabel", { amount })}
            />
            <span className="shrink-0 text-xs text-gray-500">
              {t("lifeAuction.maxBid", { max: maxBid })}
            </span>
          </label>
        </div>
      ) : null}
    </ChoiceOverlay>
  );
}
