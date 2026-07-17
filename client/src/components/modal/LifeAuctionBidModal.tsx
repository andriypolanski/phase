import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";

import { ChoiceOverlay, ConfirmButton } from "./ChoiceOverlay.tsx";
import { useGameDispatch } from "../../hooks/useGameDispatch.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import type { WaitingFor } from "../../adapter/types.ts";
import { gameButtonClass } from "../ui/buttonStyles.ts";

type LifeAuctionBid = Extract<WaitingFor, { type: "LifeAuctionBid" }>;

/**
 * Card-defined open-bid life auction. The engine exposes the standing high bid,
 * optional next legal raise, and pass/raise actions; bids are announced amounts
 * and may exceed the bidder's current life total.
 */
export function LifeAuctionBidModal({ data }: { data: LifeAuctionBid["data"] }) {
  const { t } = useTranslation("game");
  const dispatch = useGameDispatch();
  const legalActions = useGameStore((s) => s.legalActions);
  const canPass = legalActions.some((action) => action.type === "PassLifeAuction");
  const canRaise = data.next_legal_bid !== null
    && legalActions.some((action) => action.type === "SubmitLifeAuctionBid");
  const minBid = data.next_legal_bid ?? data.high_bid;
  const [amount, setAmount] = useState(minBid);

  useEffect(() => {
    if (data.next_legal_bid !== null) {
      setAmount(data.next_legal_bid);
    }
  }, [data.next_legal_bid, data.player]);

  const handlePass = useCallback(() => {
    if (!canPass) return;
    dispatch({ type: "PassLifeAuction" });
  }, [canPass, dispatch]);

  const handleBid = useCallback(() => {
    if (!canRaise || data.next_legal_bid === null || amount < data.next_legal_bid) return;
    dispatch({ type: "SubmitLifeAuctionBid", data: { amount } });
  }, [amount, canRaise, data.next_legal_bid, dispatch]);

  const highBidderLabel = t("lifeAuction.highBidder", { number: data.high_bidder + 1 });
  const bidValid = canRaise && data.next_legal_bid !== null && amount >= data.next_legal_bid;

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
            <ConfirmButton
              onClick={handleBid}
              disabled={!bidValid}
              label={t("lifeAuction.bid", { amount })}
            />
          ) : null}
        </div>
      }
    >
      {canRaise ? (
        <div className="mx-auto mb-2 w-full max-w-md px-2">
          <label className="flex flex-col gap-2 text-sm text-gray-200">
            <span className="font-mono text-base text-cyan-300">
              {t("lifeAuction.bidLabel", { amount })}
            </span>
            <input
              type="number"
              min={data.next_legal_bid ?? undefined}
              value={amount}
              onChange={(event) => setAmount(Number(event.target.value))}
              className="w-full rounded-md border border-gray-600 bg-gray-900 px-3 py-2 font-mono text-cyan-200"
              aria-label={t("lifeAuction.bidLabel", { amount })}
            />
          </label>
        </div>
      ) : (
        <p className="px-2 text-center text-sm text-gray-300">
          {t("lifeAuction.maxBidReached")}
        </p>
      )}
    </ChoiceOverlay>
  );
}
