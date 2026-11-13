# Troubleshooting

## Collecting logs

Run Ferry from a terminal with debug logging:

```bash
RUST_LOG=ferry=debug ferry
```

The log shows each step of the connection: the service search, the MAP and
PBAP sessions, the phone connecting for notifications, and every message
downloaded.

## Common problems

**My iPhone isn't in the list of devices.** Ferry lists paired devices only.
Pair the iPhone in the system's Bluetooth settings first; see
[Setup](setup.md).

**"The phone doesn't offer this service".** The iPhone hides its messages or
contacts until they're allowed: open **Settings › Bluetooth**, tap **ⓘ** next
to your computer, and turn on **Show Notifications** and **Sync Contacts**.

**"The phone is out of range, or its Bluetooth is off".** Bring the phone
closer and unlock it. Ferry keeps trying in the background, with a growing
delay; **Retry Now** in the status menu tries at once.

**New messages arrive late.** Ferry learns of new messages from the phone as
they arrive, once the phone connects back to Ferry's notification service;
until then it checks every 20 seconds. Look for
`the phone connected for notifications` in the log. On Linux, BlueZ's `obexd`
publishes a notification service too, and the phone may pick it instead.
Stop it while using Ferry with `systemctl --user stop obex`, or turn it off
for good with `systemctl --user mask obex`.

**Contacts are missing.** Turn on **Sync Contacts** for your computer in the
iPhone's Bluetooth settings, then choose **Reload Contacts** in the status
menu. Contact photos load a few seconds after the names; the log reports how
many arrived.

**Photos show as cards instead of pictures.** iPhones usually don't send
attachments over Bluetooth, and send text such as "Attachment: 1 Photo"
instead, which Ferry shows as a photo card. Messages sent as MMS with their
pictures show inline. The debug log shows what the phone sent for each message
with attachments.

**On Windows, Ferry can't connect.** Quit Microsoft's Phone Link if it's
connected to your iPhone.

## Known limitations

- iPhones only share recent messages over Bluetooth. Ferry keeps every
  message it has seen, so its history grows from the day you start using it,
  but it can't read older conversations.
- Conversations are one-to-one: messages in group conversations appear in
  the sender's conversation.
- Only photos show inline; other attachments open in the system's viewer, and
  HEIC photos aren't decoded.
- What an iPhone allows over Bluetooth depends on its iOS version. When the
  phone refuses something, such as sending a message, Ferry shows why.
