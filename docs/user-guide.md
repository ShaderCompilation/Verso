# Verso User Guide

Verso is a calm EPUB reader for jailbroken Kindles. This guide covers installing
it, reading, organising your library, looking words up, transferring
books over Wi-Fi, importing from KOReader, updating and reporting problems.

---

## 1. Install, launch and uninstall

**Requirements:** a jailbroken Kindle on **hard-float firmware (5.16.3+)** with
the **Universal Hotfix** (for direct library launch — no KUAL needed), **KPM**
(recommended on 5.19.4+ where KUAL is broken) or **KUAL**. Soft-float Kindles
are refused before touching the Kindle UI.

1. Unpack `verso-<version>.zip` into `/mnt/us/` over USB, adding `verso/`,
   `extensions/verso/` and `documents/Verso.sh`.
2. Eject the Kindle and tap **Verso** in your Kindle library.

**Optional alternatives:**

- **KPM** (useful on firmware 5.19.4+ if `.sh` items don't index in the library,
  or if you prefer a package manager): copy `verso-<version>.kpkg` to `/mnt/us/`
  over USB, eject, run `;kpm install file:///mnt/us/verso-<version>.kpkg` in the
  Kindle search bar (or `kpm install file:///mnt/us/verso-<version>.kpkg` over
  SSH / kterm), and launch with `;kpm launch verso`.
- **KUAL:** open **KUAL → Verso → Start Verso** (included in the same zip).

While Verso runs, its launcher pauses Amazon's UI and USB drive daemon so your
book storage stays mounted. Exiting (or any crash) always restores Amazon's home
screen. Verso refuses to start if KOReader is already running or the Kindle is
asleep.

- **Connecting USB while reading:** plugging into a computer opens a prompt
  with **Exit to connect to computer** (returns to Amazon's UI for USB transfer)
  and **Keep reading** (charges while staying in Verso). Wall chargers charge
  silently without prompting.
- **Where files live:** app and logs in `/mnt/us/verso/`, user data in
  `/mnt/us/verso/data/`, library launcher in `/mnt/us/documents/Verso.sh`, KUAL
  in `/mnt/us/extensions/verso/`, reports in `/mnt/us/verso/reports/`.
- **First launch:** a short setup (Welcome, Library folders, tap zones, page
  layout, KOReader import when KOReader is installed, gestures) runs over Home.
  Every choice saves at once and can be skipped; **Settings → Advanced → Run
  setup again** reopens it. Folders holding EPUBs are found and ticked for you.
- **Uninstall:** first copy `/mnt/us/verso/data/` to your computer if you want to
  keep your progress, highlights, notes and exports. Then delete `/mnt/us/verso`,
  `/mnt/us/extensions/verso` and `/mnt/us/documents/Verso.sh` over USB (or run
  `;kpm uninstall verso`, which keeps `/mnt/us/verso/data/`). Verso never touches
  system files or your books.

---

## 2. Home and Library

Verso opens on **Home**:

- **Top bar:** Battery indicator (tap to open the Top Drawer), **Search**,
  **Rescan**, **Wi-Fi transfer**, **Settings** (shows a dot when an update is
  available) and **Exit**.
- **Continue reading:** the most recently opened book that is in progress (or
  being re-read), with cover, title, author, progress bar and percentage.
- **Recent books:** up to four recently opened books (three beside Continue
  reading in landscape).
- **Start reading:** until a second book is opened, up to eight books to start with,
  newest files first (one row under Continue reading).
- **No books:** *Finding your books…* while the first scan runs; when it finds no
  EPUBs, **Send books over Wi-Fi** and **Choose folders** (Settings → Library).
- **Notice bar:** appears above the Library card for an available update or an
  unexpected crash report.
- **Library card:** tap to open the folder browser.

### Folder browser

By default, Verso scans `/mnt/us/documents`, plus any folders setup found books
in, recursively for `.epub` files (ignoring dotfiles, `.sdr` folders and symlinks).
Manage scanned folders in **Settings → Library → Add folder**; removing a folder
hides its books without deleting any files or reading progress.

- **Ordering:** immediate subfolders appear first alphabetically, followed by
  books in alphabetical title order. Empty folders are shown.
- **Navigation:** tap a folder to enter it, tap the back arrow to go up, or tap
  the current path to see its full location. Swipe left/right or tap the pager
  arrows to turn pages; tap the page counter (`1 of N`) for a **Go to page**
  slider.
- **Folder previews** (**Settings → Display → Folder previews**): **Stack**
  (default, three stacked covers with the most recently read in front),
  **Latest** (most recent cover under a folder tab), or **Icon** (folder icon
  and book count).
- **Finished books:** finished covers display a dark check badge and a full
  progress bar. Finishing a book is sticky — opening it again keeps the badge
  and starts **re-reading** with live progress until you choose **Mark
  unfinished**.
- **Rescan (`↻`):** incrementally picks up added, moved or changed EPUBs.

### Search and touch keyboard

Tap **Search** (`🔍`) on Home or inside any folder to filter by title, author or
filename across **All books** or **This folder** (including subfolders). Accents
don't matter: `di tim` finds *Đi tìm* and `елка` finds *Ёлка*.

The touch keyboard provides **English**, **Русский** and **Dansk** layouts
(cycle with the globe key), Shift (double-tap for Caps Lock), `123`/`ABC`
symbols, cursor arrows (`←`/`→`) and a submit key.

### Book actions, multi-select and moving

Long-press any book or folder to open its action menu:

- **Book:** *Open*, *Details* (metadata, language, path, reading status and
  finished date), *Synopsis* (when present in the EPUB), *Mark finished* /
  *Mark unfinished*, *Remove from continue reading* (on Home), *Select*,
  *Move* and *Delete…* (removes the EPUB, reading state, cached cover and its
  KOReader `.sdr` metadata).
- **Folder:** *Open*, *Details*, *Search in this folder*, *Mark all finished* /
  *Mark all unfinished*, *Select*, *Move*, *New folder…*, *Rename…* and
  *Delete…* (confirms recursive book and other-file counts). Configured library
  root folders cannot be selected, moved, renamed or deleted.

Choose **Select** to tick multiple items across pages (**Select all**, **Move**,
**Delete…**). Choose **Move** to carry items to another folder and tap
**Paste**; name clashes offer **Skip** or **Keep both** (with **Apply to all**).
Moving a book preserves its reading position and KOReader `.sdr` folder.

---

## 3. Reading

Verso reads **EPUB** (`.epub`, EPUB 2 and EPUB 3).

### Turning pages and menus

- **Tap zones** (**Settings → Reading → Tap zones**):
  - **Standard** (default): left 30% goes back, right/top/bottom goes forward.
  - **Left-handed:** right 30% goes back, the rest goes forward.
  - **Halves:** left half back, right half forward.
  - The centre box always opens the **Reader Menu**.
- **Swipes:** swipe left for next page, right for previous page, up from the
  bottom edge for the **Reader Menu**, or down from the top edge for the **Top
  Drawer**.
- **Page-turn buttons:** on an Oasis, the top button goes to the next page and the
  bottom one back; in landscape, right is next and left is back. **Settings →
  Reading → Swap page buttons** turns them round (remotes aren't affected).
- **Remotes:** Verso doesn't pair remotes, but reads the ones the Kindle has.
  Amazon's **Kindle Click** needs no setup. With **HID Passthrough**, a paired
  remote's page, arrow, volume, Enter and Space keys turn pages. To choose what each
  button does, bind it in **Button Mapper** to the **Auto** tab's *Next page* /
  *Previous page* (1.6.0 or later): the same binding then works in the Kindle
  reader, KOReader and Verso. The **Kindle** tab's *Next page* doesn't reach Verso,
  and its *(tap)* actions follow your tap zones, so prefer **Auto**.
- **Bookmark corner:** tapping the top-right corner toggles a page bookmark
  (configurable in **Settings → Reading**).
- **Back pill:** after jumping via Contents, Notes, chapter arrows, the progress
  slider or an internal link, a bottom pill (`← Back to p. N`) returns in one
  tap and disappears once you resume turning pages.

### Top Drawer

Pull down from the top edge anywhere (or tap the battery icon on Home/Library,
or `☀` / battery in the Reader Menu) to open the **Top Drawer**:

- **Light** and **Warmth** sliders (0–24; Warmth is hidden on models without
  warm LEDs).
- Quick tiles for **Wi-Fi**, **Orientation** (Portrait / Landscape), **Refresh**
  (full-screen flash to clear ghosting), **Wi-Fi transfer**, **Settings**, and
  **SSH**.

### Reader Menu, Contents, Search and Footnotes

- **Reader Menu:** top bar has back (`←`), book title, **Bookmark ribbon**,
  **Search**, light shortcut and battery. Bottom panel shows chapter title,
  progress, a jump slider with previous/next chapter buttons (`‹` / `›`), and
  four buttons: **Contents**, **Notes**, **Text** and **Layout**.
- **Contents:** paged table of contents indented by level, opening at the
  current chapter (`•`).
- **Search in book:** streams matches with chapter, page and context snippet;
  tapping a result shows a bottom bar (Prev / Next / Results / Return / Close).
- **Footnotes & links:** tapping a footnote marker opens a paged **Note** sheet
  (with a **Go to note** button to jump to the full passage). Other internal
  links jump directly with the back pill.
- **End of book:** turning onto the last page marks the book **Finished**.
  Tapping forward again opens the **Finished** sheet with **Undo**, **Next**
  (the next book in the same folder, if any) and **Home**.

---

## 4. Typography and page layout

Typography and page layout are **global** across your library so books reopen
instantly from cache.

- **Text** (Reader Menu → **Text**, live preview above the sheet):
  - **Font:** bundled **Literata** (default), the Kindle's built-in fonts
    (Bookerly, Caecilia, Baskerville, Palatino, etc.), and any `.ttf`/`.otf`
    files you drop into `/mnt/us/fonts/`. Books with embedded fonts also list
    **Publisher font** first.
  - **Size:** 6–24 pt in 1 pt steps (hold to repeat).
  - **Spacing:** Tight, Normal or Loose.
  - **Justify:** justified or left-aligned.
- **Layout** (Reader Menu → **Layout**, full-screen editor applied on close):
  - **Top bar** (default Off) and **Bottom bar** (default On: chapter title, page / total, percent): three tappable
    slots each (left, centre, right). Pick *Nothing* or any of 13 items:
    *Chapter title*, *Chapter and pages left*, *Book title*, *Author*, *Pages
    left in chapter*, *Page in chapter*, *Time left in chapter*, *Percent*,
    *Page / total*, *Time left in book*, *Clock (24 h)*, *Clock (12 h)*,
    *Battery*. Time-left estimates learn your reading pace automatically.
  - **Progress line:** Off, Top or Bottom (default), with optional **Chapter marks**.
  - **Margins:** Side (default 3 mm), Top (2 mm) and Bottom (2 mm), each
    2–20 mm. Landscape centres the text column to keep line length comfortable.
- **Refresh interval** (**Settings → Display → Full page refresh**): *Every
  page*, *Every 6*, *Every 12* or *Never* (default). Chapter starts and images
  always flash cleanly; rapid flipping defers periodic flashes so turns stay
  responsive.
- **Text shaping** (**Settings → Advanced**): *Light* (default, HarfBuzz
  kerning without ligatures), *Full* (full HarfBuzz ligatures and complex
  scripts) or *FreeType* (fastest layout, simpler spacing). Applies from the
  next book open.

---

## 5. Highlights, notes, bookmarks and export

- **Single word:** long-press a word and lift to open the dictionary card
  (which also has *Highlight*, *Note*, *Search* and *Extend* buttons along the
  bottom).
- **Passage:** long-press and drag — or adjust the start/end handles after
  lifting — to open the floating bar: **Highlight**, **Note**, **Search**,
  **Dict**, **Wiki**, **Translate**, **Extend**.
- **Cross-page highlight:** tap **Extend** to turn pages forward or back and
  tap where the passage ends.
- **Editing:** tap any highlight on the page to view, add/edit/delete its note
  or remove the highlight. Toggle page bookmarks with the top-right corner or
  the Reader Menu ribbon.
- **Notes screen** (Reader Menu → **Notes**): lists highlights and bookmarks in
  reading order with an **All · Highlights · Bookmarks** filter, **Import from
  KOReader** and **Export**.
- **Export:** writes UTF-8 Markdown to
  `/mnt/us/verso/data/exports/<Title> (<id>).md` grouped by chapter. Entries
  found in the text have their page in the current layout (*p. 57 of 412*). Highlights are also
  downloadable per book or all at once as a zip from the Wi-Fi Transfer web app,
  without page numbers.

---

## 6. Dictionaries, Wikipedia and Translate

The lookup card has three tabs. **Dictionary** works offline.

Verso reads **StarDict** dictionaries (`.ifo`/`.ifo.gz`, `.idx`/`.idx.gz`,
`.dict`/`.dict.dz`, optional `.syn`) with plain text, HTML, XDXF and Pango
formatting.

### Installing dictionaries

- Put unpacked StarDict folders in `/mnt/us/verso/data/dictionaries/`, or
  upload `.zip`, `.tar.gz`, `.tar.bz2` or loose files in the Wi-Fi Transfer web
  app's **Dictionaries** tab.
- On the Kindle, Verso also includes `/mnt/us/koreader/data/dict` by default so
  existing KOReader dictionaries work immediately. Add or remove folders in
  **Settings → Library → Dictionaries → Folders** and tap **Rescan** after
  copying files by USB.

### Sets, languages and word forms

Open **Settings → Library → Dictionaries** (or **Manage dictionaries** from any
lookup) to enable, disable and reorder dictionaries, create language-specific
**sets** and assign language codes. Verso automatically picks a book's set
override, its detected language, the word's script or **Default**.

Inflected words are matched back to dictionary headwords automatically:

- **English:** built-in rules and WordNet 3.0 irregular exceptions.
- **Russian:** bundled OpenCorpora pack (`/mnt/us/verso/wordforms/ru.vwf`,
  handles `ё`/`е` and stress marks).
- **Other languages:** copy UTF-8 or ISO-8859-1 **Hunspell** `.aff` and `.dic`
  files (named by language, e.g. `de_DE.aff`/`.dic`, up to 16 MiB) into
  `/mnt/us/verso/data/hunspell/` (or one subfolder below it).

### Using the lookup card

Tap the query in the header to edit or type a word; use `‹` / `›` or tap the
dictionary name to switch matches; tap `⤢` to expand in portrait. **Tap any
word inside a definition** to look it up recursively (up to 24 levels deep;
`←` steps back).

The card opens expanded at first, then at whatever size you last left it with
`⤢`. The smaller card stays at the bottom (on the right in landscape); turn
off **Always at the bottom** in **Dictionaries → Lookup card** to have it open
away from the word, so the word stays visible.

### Wikipedia and Translate

Both use the Wi-Fi you already have on, and send the word or passage to
Wikipedia or Google only when you open their tab; if that fails, tap **Retry**.

- **Wikipedia** opens a clear match or lists the results (**Other matches**
  switches). It uses the book's language, else English; a choice is kept per book.
- **Translate** uses Google Translate by default (up to 4,000 characters), or an
  optional **DeepL** Free/Pro API key (**Settings → Advanced → Translate**, or
  paste it in Wi-Fi transfer's **Translate** tab — `:fx` keys select Free
  automatically, and unsupported languages or an empty key fall back to Google).
  It asks for a target language the first time and detects the source. Google
  may refuse some shared or VPN connections as busy.

Dictionaries take a passage of any length, Wikipedia up to 300 characters.

---

## 7. Wi-Fi transfer web app

Start from the **Transfer** (`⇄`) icon on Home/Library, **Top Drawer → Wi-Fi transfer**
or **Settings → Device → Wi-Fi transfer**.

Verso turns Wi-Fi on if needed (turning it back off when you leave if it turned
it on), opens port 80 temporarily, advertises **`http://verso.local`** over
mDNS and shows a **4-digit pairing code** and **QR code** (scanning the QR code
signs in automatically; otherwise enter the code at `http://verso.local` or the
shown IP address). The server stops on **Done**, sleep, USB exit or after 10
minutes idle.

- **Books:** drag-and-drop EPUBs into any folder (with **+ New folder**; the
  library folder with the most books is preselected), search (accents don't matter), download or delete books. Switch to **Highlights** to download
  per-book Markdown or **Download all (.zip)**.
- **Dictionaries:** upload StarDict archives or loose files and remove
  installed dictionaries (rescans automatically on **Done**).
- **Files:** browse `/mnt/us` with shortcuts (*Books*, *Fonts*, *Dictionaries*,
  *Exports*, *Reports*, *Everything*), upload, download, rename, move and delete. System
  and jailbreak paths are hidden, Verso's own app and live data are read-only
  unless developer mode is on, and deleting library or KOReader folders asks
  for a second confirmation.
- **Translate:** switch between Google Translate and DeepL and paste or remove a
  DeepL API key without typing it on e-ink.

---

## 8. Importing from KOReader

All KOReader imports are **one-way** (KOReader → Verso); Verso never modifies
KOReader's files.

- **On book open** (on by default; **Settings → Reading → Check KOReader
  progress**): if `Book.sdr/metadata.epub.lua` has a newer reading position on
  a different page, Verso offers a card with **Go there** and **Stay here**.
- **Bulk library import** (**Settings → Library → KOReader → Import**):
  scans every indexed book's `.sdr` sidecar and
  `/mnt/us/koreader/history.lua`, previews how many books will update or be
  marked finished and how many new highlights, notes and bookmarks it found,
  and imports them in one pass without overwriting newer Verso positions or
  re-marking books you explicitly unmarked. Highlights are placed when each
  book next opens; a bookmark whose page isn't in the book is dropped.
- **One book:** open it and choose **Reader Menu → Notes → Import from
  KOReader** to preview its highlights and bookmarks against the open book.
  Either import skips what the other already brought over, and what you
  removed.

---

## 9. Updates and rollback

Verso checks GitHub Releases over HTTPS when it starts or the Kindle wakes, at
most every 6 hours and only over Wi-Fi that is already on (**Settings → Device →
Check automatically**), or on demand via **Check for updates**.

- Every release manifest is verified with a built-in **minisign** key before
  parsing, and every downloaded component is checked by SHA-256.
- Downloads require at least 20% battery (or a charger) and free storage space,
  keep the Kindle awake while transferring, and resume on retry.
- When ready, the launcher swaps only the changed components using a crash-safe
  journal, keeps the previous version in `/mnt/us/verso/update/previous/` and
  starts the new build on a 2-minute trial. If it crashes, hangs or fails twice
  across power loss, the launcher **automatically rolls back** and explains why.
- To revert manually at any time, open **Settings → Device → Go back**.

---

## 10. Settings, sleep screen and problem reports

**Settings** has five tabs:

- **Reading:** tap zones, top-right bookmark corner, check KOReader progress.
- **Display:** orientation, full page refresh interval, folder previews.
- **Library:** library folders, dictionaries, KOReader bulk import.
- **Device:** sleep screen (**Book cover** [default, falls back to Amazon when
  a book has no cover] or **Amazon**), Wi-Fi toggle, Wi-Fi transfer, updates.
- **Advanced:** text shaping, **Translate** (Google / DeepL provider and API
  key), **Run setup again** and **Create problem report**, plus the built-in
  **SSH server** switch (Dropbear + `sftp-server`, port 2222, key-only login via
  `/mnt/us/verso/ssh/authorized_keys`, advertised as `verso.local`). Tapping the
  version line in **Device** five times toggles developer mode, allowing
  live-data edits in Wi-Fi transfer's Files tab.

### Problem reports

Open **Settings → Advanced → Create problem report** (or **KUAL → Verso → Create
problem report** if Verso cannot start) to write
`/mnt/us/verso/reports/report-YYYYMMDD-HHMMSS.txt`. Crash reports
(`crash-*.txt`, newest five kept) are also saved automatically when Verso exits
unexpectedly.

Reports include a summary of recent sessions, version, update state,
non-identifying settings, up to 1,000 log lines and hardware diagnostics.
Book paths, library folders, API keys, the device's name and network addresses
are automatically omitted or replaced (`<book N>` / `<redacted>`). Dictionary
names, connected remote names, and names inside a faulty book (chapter files,
fonts) can remain, so review the text file before sharing it publicly.
