#!/usr/bin/env python3
"""Telegram debate example — sends output to both console and Telegram.

Kerness ships no Telegram channel.  A chat transport is an interface choice
that belongs to the program using the framework, not to the framework, so this
file owns its own `TelegramChannel`: the whole integration is one subclass of
the two-method `Channel` interface plus `pip install python-telegram-bot`.

Any other destination — Slack, a webhook, a database — is the same shape.
"""

import asyncio
import logging
import os
import sys

import kerness


class TelegramChannel(kerness.Channel):
    """Sends messages to a Telegram chat.

    Delivery failures are logged rather than raised.  Wrapped in a
    `MultiChannel`, a raise would be caught anyway; used on its own, a network
    blip should not abort a paid-for run mid-turn.
    """

    def __init__(self, token: str, chat_id: int | str) -> None:
        from telegram import Bot

        self._bot = Bot(token=token)
        self._chat_id = chat_id

    def send(self, sender: str, message: str) -> None:
        self._post(f"[{sender}] {message}")

    def send_system(self, message: str) -> None:
        self._post(f"[System] {message}")

    def _post(self, text: str) -> None:
        from telegram.error import TelegramError

        async def _send() -> None:
            await self._bot.send_message(
                chat_id=self._chat_id, text=text, disable_web_page_preview=True
            )

        try:
            asyncio.run(_send())
        except (TelegramError, RuntimeError) as exc:
            logging.warning("TelegramChannel send failed: %s", exc)


def main() -> None:
    api_key = os.environ.get("OPENROUTER_API_KEY", "")
    tg_token = os.environ.get("TELEGRAM_BOT_TOKEN", "")
    tg_chat_id = os.environ.get("TELEGRAM_CHAT_ID", "")

    if not api_key:
        print("Set OPENROUTER_API_KEY environment variable first.")
        sys.exit(1)
    if not tg_token or not tg_chat_id:
        print("Set TELEGRAM_BOT_TOKEN and TELEGRAM_CHAT_ID environment variables.")
        sys.exit(1)

    telegram_ch = TelegramChannel(token=tg_token, chat_id=int(tg_chat_id))
    channel = kerness.MultiChannel(kerness.ConsoleChannel(), telegram_ch)

    session = kerness.Session(
        gameplan="debate",
        topic="Should AI systems be open-sourced by default?",
        provider=kerness.OpenRouterProvider(api_key=api_key),
        channel=channel,
    )

    session.add_agent("Alice", model="openai/gpt-4o", persona="Open-source advocate")
    session.add_agent("Bob", model="anthropic/claude-sonnet-4", persona="Security researcher")
    session.add_agent("Moderator", model="openai/gpt-4o", role="orchestrator")

    result = session.run()

    print("\n--- Result ---")
    print(f"Topic: {result.topic}")
    print(f"Rounds: {result.rounds_run}")
    print(f"Consensus: {result.consensus_reached}")
    if result.summary:
        print(f"Summary: {result.summary}")


if __name__ == "__main__":
    main()
