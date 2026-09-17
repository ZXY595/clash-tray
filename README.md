# clash-tray

[中文](README.zh-CN.md) · English

A tiny Linux desktop tray for controlling the [mihomo](https://wiki.metacubex.one) core.
It provides its menu through StatusNotifierItem, so your desktop environment needs to
support tray icons (KDE, GNOME's AppIndicator extension, waybar or another status bar, …).

## Features

- Shows the core version and the current mode in the menu and the tooltip
- **Mode**: switch between Rule / Direct / Global
- **Proxies**: pick a node per proxy group
- **Rules**: lists every rule (`type content → target`); click one to temporarily
  disable/enable it (it is restored after the core restarts)
- **Kernal Options**: reload the config, update the GEO database, flush the fake-IP / DNS
  cache, restart the core
- When the core is unreachable, the reason is shown right in the menu

Refreshing is driven mainly by user actions: opening the menu or clicking any item
refetches immediately, and otherwise a fallback poll runs every 30 seconds (to notice core
state changed by the dashboard or another client), so it barely uses any CPU while idle.

## Running

```sh
cargo run --release
```

By default it connects to `127.0.0.1:9090`, i.e. the `external-controller` in your core config:

| Environment variable | Default | Description |
| --- | --- | --- |
| `CLASH_CONTROLLER` | `127.0.0.1:9090` | Controller address; a full `http://host:port` also works |
| `CLASH_SECRET` | empty | Matches `secret` in the config; when set, `Authorization: Bearer` is sent |

### Autostart

`clash-tray.desktop` is an XDG autostart entry. To have the tray come up with your session,
install the binary in your `$PATH` and copy the entry into place:

```sh
install -Dm644 clash-tray.desktop ~/.config/autostart/clash-tray.desktop
```

A packager can install the same file into `/etc/xdg/autostart/` for every user of the
machine. A copy in `~/.config/autostart/` under the same name takes precedence over it, so
users can still opt out or change how the tray is launched.
