# `encore.devices`

<!-- Written by tools/api_docs.py from its EXTENSIONS, the handlers in src/ and docs/api/shapes/: edit those, then run it. -->

Your devices see each other, and one can play on another (remote control). Each device says hello and its state; the others list them and send commands, which the device picks up by polling (a long-poll).

## `deviceHello`

A device announces itself (and is listed for a while after).

Parameters: `id`, `name`, `kind`.

| Field | Type |
|---|---|
| `devices` | object |
| `devices.device` | array |
| `devices.device[]` | object |
| `devices.device[].client` | string |
| `devices.device[].id` | string |
| `devices.device[].kind` | string |
| `devices.device[].name` | string |
| `devices.device[].online` | boolean |
| `devices.device[].seen` | number |
| `devices.device[].state` | object |

## `deviceState`

What it's playing, and where.

Parameters: `id`.

Answers "ok" with no fields.

## `deviceList`

Your devices, the others' state.

| Field | Type |
|---|---|
| `devices` | object |
| `devices.device` | array |
| `devices.device[]` | object |
| `devices.device[].client` | string |
| `devices.device[].id` | string |
| `devices.device[].kind` | string |
| `devices.device[].name` | string |
| `devices.device[].online` | boolean |
| `devices.device[].seen` | number |
| `devices.device[].state` | object |

## `deviceCommand`

Send a command (play, pause, next, seek, volume…) to a device.

Parameters: `target`, `id`.

| Field | Type |
|---|---|
| `deviceCommand` | object |
| `deviceCommand.online` | boolean |
| `deviceCommand.seq` | number |

## `devicePoll`

Wait for commands for this device (answers at once when one comes).

Parameters: `id`, `ack`, `since`, `wait`.

| Field | Type |
|---|---|
| `devicePoll` | object |
| `devicePoll.commands` | array |
| `devicePoll.commands[]` | object |
| `devicePoll.commands[].at` | number |
| `devicePoll.commands[].pad` | string |
| `devicePoll.commands[].sender` | string |
| `devicePoll.commands[].seq` | number |
| `devicePoll.commands[].type` | string |
| `devicePoll.devices` | array |
| `devicePoll.devices[]` | object |
| `devicePoll.devices[].client` | string |
| `devicePoll.devices[].id` | string |
| `devicePoll.devices[].kind` | string |
| `devicePoll.devices[].name` | string |
| `devicePoll.devices[].online` | boolean |
| `devicePoll.devices[].seen` | number |
| `devicePoll.devices[].state` | object |
| `devicePoll.devices[].state.at` | number |
| `devicePoll.devices[].state.playing` | boolean |
| `devicePoll.devices[].state.songId` | string |
| `devicePoll.now` | number |

## `deviceBye`

The device goes away.

Parameters: `id`.

Answers "ok" with no fields.

From 1.0, a field here may gain company but never goes away or changes type (see [STABILITY.md](../STABILITY.md)).
