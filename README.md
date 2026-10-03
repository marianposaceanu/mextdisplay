# mextdisplay

`mextdisplay` is a small terminal UI for enabling and disabling external video
outputs on Apple Silicon Macs. It removes a display from the macOS desktop while
leaving the monitor connected, so USB-C charging and the monitor's USB hub can
continue to work.

It is useful when a monitor keeps advertising a live video connection even
though its panel is off. macOS cannot reliably distinguish that state from an
intentional extended desktop, so `mextdisplay` provides a safe manual toggle
instead of guessing.

## Install

Install on an Apple Silicon Mac with macOS using [Homebrew](https://brew.sh/)
and [Marian’s tap](https://github.com/marianposaceanu/homebrew-tap):

```sh
brew tap marianposaceanu/tap
brew install mextdisplay
```

To upgrade to the latest release:

```sh
brew update
brew upgrade mextdisplay
```

### From source

Building from source requires Rust 1.88 or newer.

```sh
git clone https://github.com/marianposaceanu/mextdisplay.git
cd mextdisplay
cargo install --path .
```

Or build without installing:

```sh
cargo build --release
./target/release/mextdisplay
```

## Use

Run without arguments to open the TUI:

```sh
mextdisplay
```

Use `↑`/`↓` or `j`/`k` to select a display, `Enter` or `Space` to toggle it,
`r` to refresh, and `q` to quit. Disabling a display always asks for
confirmation.

There are also script-friendly commands. A display can be selected by UUID,
UUID prefix, numeric ID, or an unambiguous part of its name.

```sh
mextdisplay list
mextdisplay disable EV3285
mextdisplay enable BA97FB9B
```

`off` and `on` are aliases for `disable` and `enable`.

## Safety and recovery

- The built-in display, the main display, and the last active display are
  protected from being disabled.
- Changes apply only to the current macOS login session. Logging out or
  restarting restores the normal display configuration.
- Before disabling a display, `mextdisplay` atomically saves its stable UUID and
  current numeric ID. This lets it re-enable the display after macOS removes it
  from normal display enumeration.
- Recovery IDs are tied to the current boot and login session so a stale ID is
  never reused in a later session.

State is stored in:

```text
~/Library/Application Support/mextdisplay/state.json
```

## How it works

Display discovery and safety checks use CoreGraphics. Human-readable display
names come from AppKit. The actual video toggle uses the private
`CGSConfigureDisplayEnabled` CoreGraphics function because the public display
configuration API cannot disable or restore a display this way.

The private API may change in a future macOS release. If it is unavailable,
`mextdisplay` reports an error before changing the display. Power delivery and
USB behavior are controlled by the monitor and connection hardware; the tool
only changes the macOS video configuration.

## License

MIT
