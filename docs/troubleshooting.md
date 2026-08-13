# When something goes wrong

Symptom on the left, fix on the right. Start with **Help** inside the app — it
runs these checks for you and tells you which one failed.

On a headless server there is no **Help** screen and none of the menus below
exist; [`server.md`](server.md#when-it-goes-wrong) has the same problems with
`curl` and `journalctl` answers instead.

---

## macOS blocked it: "Apple could not verify" or "damaged"

Wired is not signed with a paid Apple certificate — it never will be, and that is
a deliberate choice, not an oversight. Your Mac says one of these about any such
app. Nothing is wrong with the file you downloaded.

1. Open **System Settings → Privacy & Security**.
2. Scroll down. There is a line saying *"Wired Terminal" was blocked*, with an
   **Open Anyway** button. Press it.
3. Confirm with **Open**.

You only do this once.

**If there is no such line, and right-click → Open does not work either**, your
Mac is refusing before it offers you the choice. Releases built before v1.0.2
land in that state. Open **Terminal** (⌘-Space, type `Terminal`) and paste this
one line, which clears the "downloaded from the internet" mark macOS attaches to
the file:

```bash
xattr -dr com.apple.quarantine "/Applications/Wired Terminal.app"
```

Then open Wired normally. If it still refuses, the copy is genuinely incomplete
— download it again, and drag it to **Applications** before opening it rather
than opening it from inside the disk image.

## Windows says "Windows protected your PC"

Same cause: an unsigned installer. Click **More info**, then **Run anyway**.

## My antivirus flagged it

Some antivirus products flag any new, unsigned program that installs other
programs — which is exactly what Wired's setup step does. If yours quarantines
Wired, add an exception for the Wired application and for the folder you chose.

If you would rather not, you can install the assistant yourself and skip Wired's
install step: Wired will find an installed agent CLI wherever it already is.

## It says "Wired isn't running"

The part that talks to your assistant stopped answering.

1. Press **Try again** on the message.
2. If that does not help, quit Wired completely — tray icon → **Quit Wired** —
   and open it again.
3. Still nothing: open **Help** and press **Copy diagnostics**. The most common
   cause is another program using the same connection, and diagnostics names it.

Wired moves to a different port by itself when 8000 is taken, so this is rarely
the problem any more.

## It stopped answering

Look at the top of the window.

- **Waiting for you** — it asked you something. Scroll down in the conversation;
  there are **Allow** and **Don't** buttons on the question.
- **Not running** — press **Start**.
- **Working on it…** — it is still going. Some tasks take minutes.

If it says **Ready** but does not reply, open the **Terminal** tab to see what the
assistant itself is showing. Sometimes it is asking a question Wired's filter did
not recognise, and you can answer it there directly.

## It never finished installing the assistant

Open the wizard again (**Help → Set it up**, or **Settings**) and press Install
once more. Press **Show the details** if it fails — the last few lines usually
say why.

Two common causes:

- **"Couldn't reach the internet to download it."** Check your connection, or a
  company network may be blocking npm.
- **"The install was blocked by file permissions."** Rare, because Wired installs
  into a folder you own. If it happens, install the assistant yourself and Wired
  will find it.

## It says it can't tell whether I'm signed in

That is honest rather than broken: both assistants can keep their credentials
somewhere Wired will not look without raising a password prompt. Press **Start**
and send it a message. If it is not signed in, it will say so itself, in the
Terminal tab.

## My phone stopped getting replies

Check **Settings → Talk to it from your phone**:

- **"Not connected"** with a message — the message is Telegram's own. "Unauthorized"
  means the code from BotFather is wrong or was revoked. Open **Use a different
  bot → Forget this bot** and paste a new one.
- **Muted** — press **Send replies to my phone again**.
- **0 phones paired** — send the bot a message and approve the code it gives you.

Pairing codes expire after an hour. Send another message to get a new one.

## I want to swap the bot, or the token leaked

**Settings → Talk to it from your phone → Use a different bot → Forget this bot.**
That throws away the code from BotFather and unpairs every phone, and puts the
paste-a-code box back. Revoke the old token in BotFather too (`/revoke`) if
someone else has seen it — Wired forgetting it does not stop the bot working for
whoever still has it.

## Someone I don't know messaged my bot

They get a code and nothing else. Nothing reaches your computer until you press
**Let them in**, so the safe thing to do is press **No**, or simply ignore it —
the code expires by itself.

If you approved someone by mistake, **Settings** lists the paired phones and lets
you remove them.

## A scheduled job never ran

- Is it **paused**? The line under its name says so.
- Was the computer asleep at the time? A missed run is rescheduled forward, not
  replayed.
- Does it say "Nothing to report — so it sent you nothing"? That is it working
  correctly. Turn off **Say nothing when there is nothing to report** if you want
  to hear from it either way.
- Two jobs never run at once. If one is still going, the next waits.

## How do I make it stop?

- **Right now:** tray icon → **Stop the assistant**, or the red **Stop
  everything** button at the top of the window.
- **Stop it starting by itself:** **Settings → Start when I log in**, off.
- **Stop it restarting when it exits:** **Settings → Always on**, off.
- **Remove everything it saved:** **Help → Erase Wired's settings and history**.
  This deletes Wired's settings, its saved conversations and its stored
  passwords. It does not touch your files, your folder, or the assistant you
  installed.
- **Remove the app:** drag it to the Trash (Mac) or uninstall it (Windows) after
  erasing, in that order — erasing first is what stops the settings and the login
  item being left behind.

## Where are the logs?

**Help → Open logs folder**, or **Settings → Open the log**.

- macOS: `~/Library/Logs/com.wired.terminal/`
- Windows: `%LOCALAPPDATA%\com.wired.terminal\logs\`
- Linux: `~/.local/share/wired-terminal/logs/`

## Where is everything else kept?

- Settings and saved passwords: `settings.json` in the app's configuration
  folder — **Settings → Open the settings file** shows you exactly where. On a
  Mac the passwords are in your keychain instead.
- Conversations: one file per day in the app's data folder.
- Your assistant's own files: wherever you told it to work.
