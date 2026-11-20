# BotKeep সেটআপ (বাংলা)

## ১. GitHub থেকে BotKeep binary তৈরি

1. এই প্রজেক্টের সব ফাইল GitHub repository-তে আপলোড/commit করো।
2. GitHub-এর **Actions** ট্যাবে যাও।
3. **Build BotKeep deployment** workflow খোলো।
4. **Run workflow** চাপো এবং build শেষ হওয়া পর্যন্ত অপেক্ষা করো।
5. সফল হলে `shitverter-botkeep-linux-x86_64` artifact ZIP ডাউনলোড করো।

## ২. BotKeep Files-এ আপলোড

Artifact ZIP extract করে নিচের ফাইলগুলো BotKeep-এর root folder-এ আপলোড করো:

- `converter-bot`
- `start-botkeep.sh`
- `.env.example`

আগে তৈরি করা Local Bot API build artifact থেকে `telegram-bot-api` executable-টিও একই root folder-এ আপলোড করো। এটি `converter-bot` নয়, আলাদা executable।

## ৩. Environment

`.env.example` কপি করে `.env` নামে রাখো। `.env`-এ বসাও:

- `TELOXIDE_TOKEN`: তোমার BotFather bot token
- `TELEGRAM_API_ID`: my.telegram.org থেকে পাওয়া API ID
- `TELEGRAM_API_HASH`: my.telegram.org থেকে পাওয়া API Hash

অন্য path settings আপাতত যেমন আছে তেমন রাখো। 1 GB disk-এ `MAX_INPUT_BYTES` ও `MAX_OUTPUT_BYTES` 209715200 (200 MiB) রাখো। পুরোনো `MAX_UPLOAD_MB` variable এই Source Code ব্যবহার করে না। API Hash বা Bot Token কাউকে পাঠাবে না।

## ৪. Startup

BotKeep → Startup → Startup command-এ লিখো:

```sh
sh ./start-botkeep.sh
```

তারপর Apply/Save করে server Restart করো।

## ৫. সফল হলে Console-এ

এ ধরনের log দেখতে হবে:

```text
Local Bot API Server process started on port 8081
Rust Video Converter Bot started
```

তারপর Telegram-এ ছোট একটি ভিডিও Document হিসেবে পাঠিয়ে পরীক্ষা করো।

## Cleanup কীভাবে কাজ করবে

- Telegram Local Bot API থেকে ভিডিও copy হওয়ার পর consumed `file_*` cache file মুছে যাবে।
- আগের crash থেকে পড়ে থাকা `file_*` cache files bot startup-এ মুছে যাবে।
- Conversion শেষে input/output temporary job folder নিজে থেকে মুছে যাবে।
- Restart-এর পর পড়ে থাকা `shitverter-*` job folders মুছে যাবে।
- Telegram session/database file মুছবে না।

BotKeep-এ 1 GB disk থাকায় `.env.example`-এ input/output limit 200 MiB রাখা হয়েছে। Disk allocation বাড়ানো ছাড়া এই limit অনেক বাড়িও না।
