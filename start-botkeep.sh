#!/bin/sh
set -eu

if [ ! -f .env ]; then
  echo "Missing .env. Copy .env.example to .env and fill in the required values."
  exit 1
fi
if [ ! -f ./telegram-bot-api ]; then
  echo "Missing telegram-bot-api executable. Upload the Linux x86_64 Local Bot API binary."
  exit 1
fi
if [ ! -f ./converter-bot ]; then
  echo "Missing converter-bot executable. Download the BotKeep build artifact."
  exit 1
fi

chmod 700 ./telegram-bot-api ./converter-bot
exec ./converter-bot
