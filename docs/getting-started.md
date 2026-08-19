# Getting started

This page is for using Wired, not for building it. There is nothing to type
into a terminal anywhere on this page. If you are a developer and you want the
API, the MCP server or the source build, [`README.md`](../README.md) is yours.

Every step below assumes a window with buttons in it. If you are installing on
a server over SSH, none of those buttons exist — read
[`server.md`](server.md) instead.

<!--
  Screenshots go here, one per numbered step. Capture at 1200×800 (the default
  window size) in dark mode. The prose below is written to work without them,
  so a missing image never leaves a step unexplained.
-->

## What Wired is

Wired keeps an AI assistant running on your own computer. You ask it for things
in plain English, and it can read and change files in **one folder that you
choose**. It keeps working after you close the window, so it can answer you from
your phone and do things on a schedule.

It is not itself the AI. It runs one you install — Claude Code, Grok, Codex
or Gemini — and
looks after it: restarting it if it stops, keeping a record of what it did, and
passing your messages to it.

## 1. Install it

Go to the [releases page](https://github.com/wired/wired-terminal/releases) and
download the file for your computer:

| Your computer | The file to download |
|---|---|
| Mac | the one ending in **`.dmg`** |
| Windows | the one ending in **`.exe`** |
| Linux | the one ending in **`.AppImage`** |

Open the file you downloaded and follow it. On a Mac, drag Wired into your
Applications folder.

**If your Mac says "Apple could not verify Wired"**, or that Wired **"is damaged
and can't be opened"**: it is not damaged. Both messages appear because Wired is
not signed with a paid Apple certificate, which is a deliberate choice. See
[Troubleshooting](troubleshooting.md#macos-blocked-it-apple-could-not-verify-or-damaged)
for the way past each one.

**If Windows shows a blue "Windows protected your PC" box:** click **More info**,
then **Run anyway**. This appears for the same reason.

## 2. Let it set itself up

The first time you open Wired it asks you four things. Each one is a button.

1. **What this is.** A page explaining what the assistant can touch, what
   "always on" means, and how to stop it. Read it — it is short and it is the
   honest version.
2. **Install your assistant.** Pick Claude Code, Grok, Codex or Gemini, then
   press Install.
   Wired downloads it for you. If it tells you Node.js is missing, press the
   button it gives you, install Node.js from the page that opens, come back and
   press **Check again**.
3. **Sign in.** Press **Open the sign-in**. Your assistant's own sign-in appears
   in a black panel at the bottom of the window; answer its questions there. It
   may open your web browser — that is normal. When it is done, press **I'm
   signed in**.
4. **Choose a folder.** This is the only folder your assistant may read and
   write. Wired suggests a new folder called `Wired` inside your home folder,
   which is a good choice if you have no better one. Press **Browse…** to pick a
   different one.

That is the whole setup.

## 3. Ask it something

The main screen is a conversation. Type at the bottom and press **Enter**. Three
suggestions appear before you have said anything — click one to try it.

Underneath the box, Wired always shows which folder it is working in. That is the
extent of what it can touch.

While it works, the top of the window says **Working on it…**. It acts in the
folder you chose without stopping to ask.

## 4. Reach it from your phone

This takes about a minute and needs no technical setup at all — no ports, no
router settings, nothing typed into a web address.

1. On your phone, open Telegram and search for **@BotFather**. It is Telegram's
   own account for making bots.
2. Send it `/newbot`. It asks for a name and a username; answer both. It replies
   with a long code.
3. In Wired, go to **Settings → Talk to it from your phone** and paste that code
   in. Press **Connect**.
4. Telegram will now show you a bot with the name you chose. Open it and send it
   anything — "hello" is fine.
5. It replies with an 8-character code. Back in Wired, that code appears with a
   **Let them in** button. Press it.

Done. Anything you send the bot goes to your assistant, and its answers come
back to your phone.

Commands the bot understands: `/status`, `/stop`, `/mute`, `/unmute`, `/help`.

## 5. Have it do things on its own

**Schedule** is where you tell it to do something regularly. Press **Add
something**, describe the job, and say when in ordinary words:

- `every hour`
- `every morning at 8`
- `every monday at 9am`
- `every 30 minutes`

Three examples are pre-filled the first time, so you can see the shape of it.

Leave **Say nothing when there is nothing to report** switched on. It means a
job that finds nothing sends you nothing, which is the difference between a
useful assistant and one you end up muting.

Press **Run it now** to try a schedule before trusting it to a timetable.

## 6. Keeping it running

Closing the window does **not** stop your assistant — that is the point. There is
a Wired icon in your menu bar (Mac) or system tray (Windows) the whole time it is
running. Click it for:

- **Open Wired** — bring the window back
- **Stop the assistant** — stop it now
- **Quit Wired** — stop everything and close

Turn on **Settings → Start when I log in** and it will be there in the morning.

## 7. When something goes wrong

Open **Help**. It checks each thing in turn — is the assistant installed, are you
signed in, can it write to your folder — and tells you which one is broken, with
a button to fix it.

If you need to ask somebody for help, press **Copy diagnostics** first and paste
what it gives you into your message. That is the information they will ask for.

[Troubleshooting](troubleshooting.md) has the common problems written out.

## What Wired can and cannot do

**It can** read and change files in the folder you chose, run programs as you,
and reach the internet — because that is what an assistant with a computer is.

**It cannot** touch anything outside that folder unless you change the folder,
and it will ask you before it acts until you tell it to stop asking.

**To stop it completely:** the tray icon → **Stop the assistant**. To remove
everything Wired saved about you: **Help → Erase Wired's settings and history**.
That leaves your own files alone.
