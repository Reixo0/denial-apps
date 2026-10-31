# Calls

Phone and contacts app for Denial phones, written with GPUI. Calls go through
dmail's `org.dmail.Voice1`: the app places, answers and ends them there and
follows its `Calls`. Mute is callaudio's. `calls-ringer`, a small user service
(`packaging/calls-ringer.service`), plays the ringtone with `pw-play` while a
call comes in and opens the app on it, and turns the screen off while a call on
the earpiece is near a face (steinerd's proximity sensor, wlr output power). While
the session is locked it also posts the call as a notification, category
`call.incoming` or `call.ongoing`, whose actions Denial's lock screen offers
without the PIN. The
call screen's controls ignore touch while near; the app tells of missed calls. Speaker
moves a call from the earpiece through callaudio, and Hold is off, as dmail has
no request for it. A second call waits unseen until the first
ends.

It is drawn to Denial's mobile materials, Islands
(denial/docs/MOBILE_APP_MATERIALS.md), as Messages is, and shares its
structure: one screen stack with the same motion, the keyboard inset, and
one Back action for Denial's Back gesture (`back`) and Escape. Content
scrolls under the bars, the gesture zone and the keyboard, never faded; no
bar moves for the keyboard, which only rises over them. A tab bar anchored
at the bottom, opaque and the window's full width, switches between three
tabs, which slide side by side:

- **Recents**: the call log in day islands (Today, Yesterday, Earlier). A
  row shows who, what happened and when, and calls back from its button.
  Tapping a row opens whom it was with; a long press lifts it with Call,
  Message, Copy and Delete, which asks first.
- **Contacts**: a favourites island, then one island per initial letter with
  the letter inside it. A long press lifts a contact with Call, Message and
  Favourite.
- **Keypad**: from the bottom, the round green Call with backspace beside it
  (a long press clears the number), the keys, each an island in a run with
  its letters under the digit (a long press on 0 types +), and the number,
  large, on its own island, with the contact it belongs to under it. Above
  them up to three suggestions: contacts whose number holds the digits or
  whose name they spell on the keys' letters (4-4-8 finds Giulia), and
  recent numbers no one has. A tap puts a suggestion's number on the keypad;
  its button calls it. Call on an empty keypad brings back the number called
  last. A hardware keyboard types on it too.

Recents and Contacts have a search bar at the top. It leaves as the list
scrolls on and comes back as soon as it scrolls back. The search bar finds
contacts by name or number; a typed number no one has is an accent island
that calls it.

A contact's detail has the avatar over the name island, Call, Message and
Share in one glass group (Share copies the name and numbers), the numbers
with their own Message and Call (a long press copies one), and the recent
calls. The star in the top group makes the contact a favourite. A number no
one has gets the same screen with Copy in place of Share.

The call screen has whom the call is with at the top, then Mute, Keypad,
Speaker, Hold, Message and Details in one glass group over End. While a call
rings here, Decline and Answer take their place. The in-call keypad sends
what it types as tones, shown under the name. Back leaves the call running:
other screens show it as a bar, over the tab bar on the tabs, with End, and a
tap on the bar goes back to it. One call at a time.

Contacts come from the contacts folder, `~/.local/share/contacts`, through
`libraries/denial-contacts`, and follow its changes. Favourites are kept on the phone, in
`$XDG_STATE_HOME/denial/calls/favourites`, and the calls in `log` beside it.

Message opens Messages on the number: `messages <number>`, from beside the
Calls binary, else from the PATH. A running Messages takes it.

`calls <number>` opens the keypad with the number on it. One Calls runs at a
time: it owns `org.denialwm.Calls` on the session bus, and a second `calls
<number>` hands the number and its activation token to it through `Open` and
exits.

## Dependencies

- `libraries/denial-look`, `libraries/denial-look-gpui` and
  `libraries/denial-contacts`, as in Messages.
- The local gpui-touch checkout, as in Messages (see its README).

## Build

On the host:

```sh
cargo run --release
```

For the Ace 3, cross-built against its sysroot in
`/mnt/development/denial-os/cross/sysroot`:

```sh
./build-aston.sh
```

Both write to `target/` at the denial-apps root.
